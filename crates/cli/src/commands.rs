//! The slash commands: everything you can ask of Letronna itself rather
//! than of the model.

use letronna_core::agents::Seen;
use letronna_core::fallback::Fallback;
use letronna_core::gate::Mode;
use letronna_core::skills::{Skill, Source};
use letronna_core::slash::{self, Origin};
use letronna_core::{goal, history, store};
use std::sync::atomic::Ordering;
use std::time::Instant;

use crate::banner::{banner, folder_name, footer, replay};
use crate::bar;
use crate::render;
use crate::session::{Runner, usage as reply_usage};

pub enum Flow {
    Handled,
    Stop,
    Unknown,
    /// Send this as the user's turn: an MCP server's prompt, filled in.
    Send(String),
    /// Send this prompt to the model, as though it had been typed.
    Ask(&'static str),
    /// Send this prompt to run a saved workflow, with the `workflow` tool
    /// granted for its turn.
    Run(String),
}

pub fn command(
    runner: &mut Runner,
    line: &str,
    session: &mut usize,
    workspace: Option<&std::path::Path>,
) -> Flow {
    let whole = line;
    // A command may carry a word after it: `/model gpt` narrows the list
    // rather than making you scroll it.
    let (line, rest) = line.split_once(char::is_whitespace).unwrap_or((line, ""));
    let rest = rest.trim();
    match line {
        "/exit" | "/quit" => Flow::Stop,
        "/help" => {
            help(workspace);
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
            runner.mode = if runner.mode == Mode::Plan {
                Mode::Default
            } else {
                Mode::Plan
            };
            letronna_core::agents::set_plan(runner.mode == Mode::Plan);
            let note = footer(runner);
            runner.bar.set_footer(note);
            render::blank();
            render::line(&render::dim(if runner.mode == Mode::Plan {
                "Plan mode on: reads, searches and runs read-only commands, never edits your files."
            } else {
                "Plan mode off."
            }));
            Flow::Handled
        }
        "/mode" => {
            render::blank();
            if !rest.is_empty() {
                match Mode::parse(rest) {
                    Ok(mode) => runner.mode = mode,
                    Err(error) => {
                        render::line(&render::red(&error));
                        return Flow::Handled;
                    }
                }
                let note = footer(runner);
                runner.bar.set_footer(note);
            }
            let mode = runner.mode;
            render::line(&render::dim(&format!(
                "{}: {}.",
                mode.label(),
                mode.about()
            )));
            if rest.is_empty() {
                let names = Mode::OFFERED.map(Mode::name).join(", ");
                render::line(&render::dim(&format!(
                    "Change it with /mode and one of {names}."
                )));
            }
            Flow::Handled
        }
        "/model" => {
            switch_model(runner, Some(*session), rest);
            Flow::Handled
        }
        "/fallback" => {
            fallback(runner, rest);
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
            let out = runner
                .bar
                .busy("signing this computer out", letronna_core::auth::sign_out);
            render::blank();
            match (who, out) {
                (_, Err(why)) => render::line(&render::red(&why)),
                (Some(account), Ok(note)) => {
                    render::line(&render::dim(&format!(
                        "Signed out {}. Your sessions and settings stay where they are.",
                        account.name
                    )));
                    if let Some(note) = note {
                        render::line(&render::dim(&note));
                    }
                }
                (None, Ok(Some(note))) => {
                    render::line(&render::dim(&format!("Signed out here. {note}")));
                }
                (None, Ok(None)) => render::line(&render::dim("Nobody was signed in.")),
            }
            Flow::Handled
        }
        "/whoami" => {
            whoami(runner);
            Flow::Handled
        }
        "/effort" => {
            effort(runner, *session, rest);
            Flow::Handled
        }
        "/style" => {
            style(runner, rest);
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
        "/notify" => {
            let settings = &mut runner.store.settings;
            settings.notifications = !settings.notifications;
            let on = settings.notifications;
            runner.persist();
            render::blank();
            render::line(&render::dim(if on {
                "You are told when a reply ends or needs you while the terminal is in the background."
            } else {
                "Notifications off."
            }));
            Flow::Handled
        }
        "/voice" => {
            voice(runner);
            Flow::Handled
        }
        "/verify" => {
            runner.store.settings.verify_edits = !runner.store.settings.verify_edits;
            runner.persist();
            render::blank();
            let settings = &runner.store.settings;
            render::line(&render::dim(if settings.verifies_edits() {
                "After changing files, the model is asked once to build or test before it finishes."
            } else if settings.verify_edits {
                "On, but run_command is switched off, so nothing is asked until it is back on."
            } else {
                "The model may finish after changing files without being asked to check them."
            }));
            Flow::Handled
        }
        "/jobs" => {
            jobs(rest);
            Flow::Handled
        }
        "/loop" | "/heartbeat" => {
            looping(runner, *session, line, rest);
            Flow::Handled
        }
        "/agents" => {
            agents(runner.store.conversations[*session].id, rest);
            Flow::Handled
        }
        "/skills" => {
            skills(runner, rest, workspace);
            Flow::Handled
        }
        "/mcp" => {
            mcp_servers(runner, rest);
            Flow::Handled
        }
        "/plugins" => {
            plugins(runner, rest);
            Flow::Handled
        }
        "/resume" => {
            resume(runner, session, workspace, rest);
            Flow::Handled
        }
        "/diff" => {
            working_diff(runner, *session, workspace, rest);
            Flow::Handled
        }
        "/import" => {
            import(runner, session, workspace, rest);
            Flow::Handled
        }
        "/undo" => {
            match with_files(rest) {
                Some(files) => undo(runner, *session, workspace, files),
                None => {
                    render::blank();
                    render::line(&render::red(
                        "Usage: /undo, or /undo --files to put the files back too.",
                    ));
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
        "/init" => {
            let conversation = &runner.store.conversations[*session];
            match letronna_core::init::prompt(conversation.workspace.as_deref()) {
                Ok(prompt) => Flow::Ask(prompt),
                Err(why) => {
                    render::blank();
                    render::line(&render::dim(&why));
                    Flow::Handled
                }
            }
        }
        "/btw" => {
            aside(runner, *session, rest);
            Flow::Handled
        }
        "/goal" => {
            let goal = &mut runner.store.conversations[*session].goal;
            let said = goal::apply_command(goal, goal::parse(rest));
            runner.persist();
            runner.show_goal();
            render::blank();
            match said {
                Ok(said) => said
                    .lines()
                    .for_each(|line| render::line(&render::dim(line))),
                Err(error) => render::line(&render::red(&error)),
            }
            Flow::Handled
        }
        "/image" => {
            attach(runner, rest, workspace);
            Flow::Handled
        }
        "/preview" => {
            preview(rest, workspace);
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
        "/usage" => {
            usage(runner.store.conversations[*session].id);
            Flow::Handled
        }
        "/insights" => {
            insights(rest);
            Flow::Handled
        }
        name if name.starts_with("/mcp__") => match mcp_prompt(runner, name, rest) {
            Some(Ok(text)) => Flow::Send(text),
            Some(Err(error)) => {
                render::blank();
                render::line(&render::red(&error));
                Flow::Handled
            }
            None => Flow::Handled,
        },
        _ => added(runner, whole, workspace),
    }
}

/// The MCP server's prompt behind `command`, filled in with `rest`, fetched
/// with the wheel turning; None once given up on with escape, when the
/// request runs on to its own time limit and its answer is dropped.
fn mcp_prompt(runner: &mut Runner, command: &str, rest: &str) -> Option<Result<String, String>> {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::mpsc::RecvTimeoutError;
    let (command, rest) = (command.to_owned(), rest.to_owned());
    let stop = std::sync::Arc::new(AtomicBool::new(false));
    let given_up = stop.clone();
    runner
        .bar
        .busy_cancellable("asking the MCP server · esc to cancel", stop, move || {
            let (done, answer) = std::sync::mpsc::channel();
            std::thread::spawn(move || {
                let _ = done.send(letronna_core::mcp::expand(&command, &rest));
            });
            while !given_up.load(Ordering::Relaxed) {
                match answer.recv_timeout(std::time::Duration::from_millis(100)) {
                    Ok(outcome) => return Some(outcome),
                    Err(RecvTimeoutError::Disconnected) => return None,
                    Err(RecvTimeoutError::Timeout) => {}
                }
            }
            None
        })
}

/// A command the user, the project or a skill added: what it gives goes in
/// the box, to be read before it is sent. A saved workflow's runs instead.
/// Unknown when none has the name.
fn added(runner: &mut Runner, line: &str, workspace: Option<&std::path::Path>) -> Flow {
    let Some((name, args)) = slash::split(line) else {
        return Flow::Unknown;
    };
    let commands = listed(workspace);
    let Some(command) = slash::find(&commands, name) else {
        return Flow::Unknown;
    };
    if command.origin == Origin::Workflow {
        // All it sends is a call to the workflow tool, whose card shows the
        // script and waits before anything runs.
        let prompt = letronna_core::workflow::saved::prompt(&command.name, args);
        return Flow::Run(prompt);
    }
    render::blank();
    render::said(
        &render::blue("●"),
        &format!(
            "{} {}",
            render::bold(&format!("/{}", command.name)),
            render::dim(&format!("· {}", from(command.origin)))
        ),
    );
    match slash::fill(command, args) {
        Ok(given) => {
            // Queued while a turn ran, it can find a next line begun, which stays.
            let given = match runner.bar.draft().trim() {
                "" => given,
                draft => format!("{draft}\n{given}"),
            };
            runner.bar.say(&given);
        }
        Err(error) => render::line(&render::red(&error)),
    }
    Flow::Handled
}

/// Offer the added commands in the `/` menu, read afresh, so a file made or
/// changed shows from the next command on.
pub fn offer_added(runner: &mut Runner, workspace: Option<&std::path::Path>) {
    let offered = listed(workspace)
        .into_iter()
        .filter(|c| c.hidden_by.is_none())
        .map(|c| {
            let blurb = match c.hint.as_str() {
                "" => c.description,
                hint => format!("{hint} · {}", c.description),
            };
            (format!("/{}", c.name), blurb)
        })
        .collect();
    runner.bar.offer(offered);
}

/// An added command's line in `/help`: where it comes from and what it does,
/// or, when another has its name, which one that is.
fn about(command: &slash::Command) -> String {
    let from = from(command.origin);
    let Some(winner) = command.hidden_by else {
        return match command.hint.as_str() {
            "" => format!("{from} · {}", command.description),
            hint => format!("{from} · {hint} · {}", command.description),
        };
    };
    let winner = match winner {
        Origin::BuiltIn => "Letronna's own",
        Origin::User => "yours",
        Origin::Project => "the project's",
        Origin::Skill => "a skill",
        Origin::Workflow => "a saved workflow",
    };
    format!("{from} · hidden, {winner} has the name")
}

/// Where an added command comes from, in a word.
fn from(origin: Origin) -> &'static str {
    match origin {
        // Only a name's winner is ever built in; the match must name it.
        Origin::BuiltIn => "built in",
        Origin::User => "yours",
        Origin::Project => "project",
        Origin::Skill => "skill",
        Origin::Workflow => "workflow",
    }
}

/// Every added command, beside Letronna's own, which win over them.
fn listed(workspace: Option<&std::path::Path>) -> Vec<slash::Command> {
    let own: Vec<&str> = bar::COMMANDS
        .iter()
        .map(|c| c.name.trim_start_matches('/'))
        .chain(["quit"])
        .collect();
    slash::list(workspace, &own)
}

/// The models a task carries on with when its own cannot answer: listed,
/// one added to the end, or all cleared.
fn fallback(runner: &mut Runner, rest: &str) {
    const USAGE: &str = "Usage: /fallback, /fallback add <model> (or Gateway:model for one on another gateway), or /fallback clear.";
    render::blank();
    let (verb, text) = rest.split_once(' ').unwrap_or((rest, ""));
    let fallbacks = &mut runner.store.settings.fallbacks;
    match (verb, text.trim()) {
        ("", "") => {}
        ("add", "") => return render::line(&render::red(USAGE)),
        ("add", text) => {
            let fallback = match Fallback::parse(text) {
                Ok(fallback) => fallback.to_string(),
                Err(error) => return render::line(&render::red(&error)),
            };
            if fallbacks.contains(&fallback) {
                render::line(&render::dim(&format!("{fallback} is a fallback already.")));
            } else {
                fallbacks.push(fallback);
                runner.persist();
            }
        }
        ("clear", "") => {
            fallbacks.clear();
            runner.persist();
        }
        _ => return render::line(&render::red(USAGE)),
    }
    let fallbacks = &runner.store.settings.fallbacks;
    if fallbacks.is_empty() {
        render::line(&render::dim(
            "No fallbacks: a task stops when its model is overloaded or out of credit. Add one with /fallback add <model>.",
        ));
        return;
    }
    render::said(&render::blue("●"), &render::bold("Fallbacks"));
    for (n, fallback) in fallbacks.iter().enumerate() {
        render::line(&format!(
            "{}  {fallback}",
            render::dim(&(n + 1).to_string())
        ));
    }
    render::line(&render::dim(&format!(
        "Asked in order once your model's own tries are spent. One that fails is passed over for {} minutes.",
        letronna_core::fallback::COOLDOWN.as_secs() / 60
    )));
}

/// What the model calls took: this session's, each recent day's and each
/// model's, added up from the saved sessions.
fn usage(session: u64) {
    use letronna_core::usage::{self, Totals};
    let report = usage::report(session);
    let days = report
        .days
        .iter()
        .map(|(date, totals)| (usage::day_name(*date), totals));
    let models = report
        .models
        .iter()
        .map(|(model, totals)| (model.clone(), totals));
    let sections: [(String, Vec<(String, &Totals)>); 3] = [
        (
            String::new(),
            vec![("this session".into(), &report.session)],
        ),
        (format!("the last {} days", usage::DAYS), days.collect()),
        (
            format!("by model, the last {} days", usage::MODEL_DAYS),
            models.collect(),
        ),
    ];
    let width = sections
        .iter()
        .flat_map(|(_, rows)| rows.iter().map(|(label, _)| label.chars().count()))
        .max()
        .unwrap_or(0);
    render::blank();
    render::said(&render::blue("●"), &render::bold("Usage"));
    for (heading, rows) in sections.iter().filter(|(_, rows)| !rows.is_empty()) {
        if !heading.is_empty() {
            render::blank();
            render::line(&render::dim(heading));
        }
        for (label, totals) in rows {
            render::line(&format!(
                "{}  {}",
                render::bold(&format!("{label:<width$}")),
                totals.line()
            ));
        }
    }
    if !report.misses.is_empty() {
        render::blank();
        render::line(&render::dim("cache misses in this session, newest first"));
        for missed in &report.misses {
            render::line(&missed.line());
        }
    }
    if let Some(legend) = report.legend() {
        render::blank();
        render::line(&render::dim(&legend));
    }
}

/// How the last weeks went: replies and tokens by day, the hours they came
/// in, the tools and models used most, and what the cache served.
fn insights(rest: &str) {
    use letronna_core::insights::{self, bar, spark};
    use letronna_core::usage::{count, day_name};
    let weeks = match rest {
        "" => insights::WEEKS,
        words => match words.parse::<u64>() {
            Ok(weeks) if (1..=insights::MAX_WEEKS).contains(&weeks) => weeks,
            _ => {
                render::line(&render::red(&format!(
                    "Weeks is a number from 1 to {}: /insights 8",
                    insights::MAX_WEEKS
                )));
                return;
            }
        },
    };
    let found = insights::insights(weeks);
    const BAR: usize = 20;
    let cached = |totals: &letronna_core::usage::Totals| {
        totals
            .cache_rate()
            .map(|rate| format!("{rate}% cached"))
            .unwrap_or_else(|| "cache not reported".into())
    };
    let row = |label: &str, width: usize, bar: String, rest: String| {
        render::line(&format!(
            "{}  {}  {}",
            render::bold(&format!("{label:<width$}")),
            render::blue(&format!("{bar:<BAR$}")),
            render::dim(&rest)
        ));
    };
    render::blank();
    render::said(&render::blue("●"), &render::bold("Insights"));
    let weeks_name = if weeks == 1 { "week" } else { "weeks" };
    let replies = found.replies();
    let mut summary = vec![
        format!("the last {weeks} {weeks_name}"),
        format!(
            "{replies} {}",
            if replies == 1 { "reply" } else { "replies" }
        ),
        format!(
            "{} tokens",
            count(found.total.input.saturating_add(found.total.output))
        ),
    ];
    if found.total.calls > 0 {
        summary.push(cached(&found.total));
    }
    render::line(&render::dim(&summary.join(" · ")));
    if replies == 0 {
        return;
    }

    render::blank();
    render::line(&render::dim("by day"));
    let days: Vec<(String, &insights::Day)> = found
        .days
        .iter()
        .rev()
        .filter(|day| day.replies > 0)
        .map(|day| (day_name(day.date), day))
        .collect();
    let most = days.iter().map(|(_, day)| day.tokens).max().unwrap_or(0);
    let width = days.iter().map(|(name, _)| name.chars().count()).max();
    for (name, day) in &days {
        let replies = if day.replies == 1 { "reply" } else { "replies" };
        let rest = format!("{} {replies} · {}", day.replies, count(day.tokens));
        row(name, width.unwrap_or(0), bar(day.tokens, most, BAR), rest);
    }

    render::blank();
    render::line(&render::dim("by hour"));
    render::line(&render::blue(&spark(&found.hours)));
    render::line(&render::dim("0     6     12    18"));

    if !found.tools.is_empty() {
        render::blank();
        render::line(&render::dim("top tools"));
        let most = found.tools.first().map_or(0, |(_, calls)| *calls);
        let width = found
            .tools
            .iter()
            .map(|(name, _)| name_label(name).chars().count())
            .max();
        for (name, calls) in &found.tools {
            let times = if *calls == 1 { "call" } else { "calls" };
            let rest = format!("{calls} {times}");
            row(
                &name_label(name),
                width.unwrap_or(0),
                bar(*calls, most, BAR),
                rest,
            );
        }
    }

    render::blank();
    render::line(&render::dim("top models"));
    let tokens = |totals: &letronna_core::usage::Totals| totals.input.saturating_add(totals.output);
    let most = found.models.first().map_or(0, |(_, totals)| tokens(totals));
    let width = found
        .models
        .iter()
        .map(|(name, _)| name_label(name).chars().count())
        .max();
    for (name, totals) in &found.models {
        let rest = format!("{} tokens · {}", count(tokens(totals)), cached(totals));
        row(
            &name_label(name),
            width.unwrap_or(0),
            bar(tokens(totals), most, BAR),
            rest,
        );
    }
}

/// A tool or model name as its row starts it: one line, cut short, and
/// without control characters.
fn name_label(name: &str) -> String {
    // The names come from the model or its gateway as sent, so an escape in
    // one would restyle the terminal and one long one would pad every row.
    render::fit(name, 40)
        .chars()
        .filter(|c| !c.is_control())
        .collect()
}

/// A side question, answered dim under it from what the session holds so far
/// and kept out of the session.
fn aside(runner: &mut Runner, session: usize, question: &str) {
    match runner.btw(session, question) {
        Ok(answer) => {
            let mut lines = answer.content.lines();
            let first = lines.next().unwrap_or_default();
            render::said(&render::dim("◇"), &render::dim(first));
            for line in lines {
                render::line(&render::dim(line));
            }
            // What it cost, as under a reply, since the session keeps no trace of it.
            render::line(&render::dim(&reply_usage(&answer)));
        }
        Err(error) => render::line(&render::red(&error)),
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
    ("//", "start a message with one /, as in //tmp is full"),
];

/// The commands, then the added ones, then the keys, each in one column so
/// it reads straight down.
fn help(workspace: Option<&std::path::Path>) {
    render::blank();
    let added: Vec<(String, String)> = listed(workspace)
        .iter()
        .map(|c| (format!("/{}", c.name), about(c)))
        .collect();
    let width = bar::name_width(&added);
    let row = |name: &str, blurb: &str| {
        // An added command's description is its writer's and can run long.
        let room = render::width().saturating_sub(width + 2);
        render::line(&format!(
            "{}  {}",
            render::bold(&format!("{:<width$}", render::fit(name, width))),
            render::dim(&render::fit(blurb, room))
        ));
    };
    for entry in bar::COMMANDS {
        row(entry.name, entry.blurb);
    }
    if !added.is_empty() {
        render::blank();
        for (name, about) in &added {
            row(name, about);
        }
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

/// Turn reading replies aloud on or off.
fn voice(runner: &mut Runner) {
    render::blank();
    let on = !runner.store.settings.voice;
    // The speech is made on the hosted models only an account can use.
    if on && !letronna_core::auth::signed_in() {
        render::line(&render::red(
            "Replies are read aloud by Letronna's hosted voice, which needs an account: \
             sign in with /login first.",
        ));
        return;
    }
    runner.store.settings.voice = on;
    runner.persist();
    runner.match_voice();
    let note = footer(runner);
    runner.bar.set_footer(note);
    render::line(&render::dim(if on {
        "Replies are read aloud as they stream, paid from your Letronna credit by the character."
    } else {
        "Replies are no longer read aloud."
    }));
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

/// How hard the model should think in this session and new ones, from the
/// levels the session's model offers.
///
/// On a model that can be told, this is the difference between sixty-four
/// reasoning tokens and seven hundred. The levels come from its gateway's
/// model list, or are low, medium and high when the list does not say.
fn effort(runner: &mut Runner, session: usize, rest: &str) {
    // This terminal only asks for the gateway's list with its first request
    // or /model, and the levels are on it.
    let (endpoint, key) = runner.route(session);
    if let Some(asked) = letronna_core::models::ask_for_list(&endpoint, &key) {
        runner.bar.busy("asking what the model offers", move || {
            letronna_core::models::wait_for_list(asked)
        });
    }
    let model = runner.store.model(session).1.to_owned();
    let saved = runner.store.effort(session).to_owned();
    let levels = letronna_core::models::efforts(&endpoint, &model);
    if levels.is_empty() {
        render::blank();
        render::line(&render::dim(&format!(
            "{model} cannot be told how hard to think; it decides for itself."
        )));
        return;
    }
    let stops: Vec<&str> = std::iter::once("auto")
        .chain(levels.iter().map(|level| level.id.as_str()))
        .collect();
    // What goes with the next request, which for a level saved on another
    // model is the nearest this one offers.
    let current =
        letronna_core::models::clamp(&levels, &saved).map_or("auto", |level| level.id.as_str());
    let chosen = match rest {
        // Nothing named: slide to it. Effort is a point on one line from
        // less to more, and a slider says so where a list would not.
        "" => {
            let start = stops.iter().position(|l| *l == current).unwrap_or(0);
            match runner.bar.slide("How hard should it think?", &stops, start) {
                // Where it already was: a level saved on a model that goes
                // further stays saved for when that model is back.
                Some(index) if stops[index] == current => saved.clone(),
                Some(index) => stops[index].to_owned(),
                None => {
                    render::blank();
                    render::line(&render::dim(&format!("Left at {current}.")));
                    return;
                }
            }
        }
        asked => asked.to_lowercase(),
    };
    // Saved as named, even a level this model lacks: that is sent as the
    // nearest it has, and as named again on a model that has it. Auto, or a
    // word that is no level, leaves it to the model.
    let sent = letronna_core::models::clamp(&levels, &chosen).map(|level| level.id.clone());
    let effort = if sent.is_some() { chosen.as_str() } else { "" };
    runner.store.choose_effort(session, effort);
    runner.persist();
    let note = footer(runner);
    runner.bar.set_footer(note);
    render::blank();
    render::line(&render::dim(&match sent {
        None => "Thinking left to the model.".to_owned(),
        Some(level) if level == chosen => format!("Thinking {level}."),
        Some(level) => format!("Thinking {level}, the nearest {model} has to {chosen}."),
    }));
}

/// The reply styles with the chosen one marked, or `name` chosen.
fn style(runner: &mut Runner, name: &str) {
    use letronna_core::styles;
    render::blank();
    if !name.is_empty() {
        let chosen = if name.eq_ignore_ascii_case(styles::DEFAULT) {
            String::new()
        } else {
            let Some(style) = styles::find(name) else {
                render::line(&render::red(&format!(
                    "No style called {name}. /style lists them."
                )));
                return;
            };
            style.name
        };
        runner.store.choose_style(&chosen);
        runner.persist();
        render::line(&render::dim(&if chosen.is_empty() {
            "Default style: replies as Letronna usually writes them.".to_owned()
        } else {
            format!("Replies in the {chosen} style from the next message.")
        }));
        return;
    }
    // From the file, as requests read it: the window may have picked since.
    // A saved name whose file has gone reads as default, so it is marked so.
    let current = styles::current().map(|s| s.name);
    let default = (
        styles::DEFAULT.to_owned(),
        "no style, as Letronna usually writes".to_owned(),
        current.is_none(),
    );
    let rows: Vec<(String, String, bool)> = std::iter::once(default)
        .chain(styles::all().into_iter().map(|style| {
            let mut about = style.about;
            if style.cut {
                about.push_str(&format!(
                    "{}cut at {} characters",
                    if about.is_empty() { "" } else { " · " },
                    styles::LIMIT
                ));
            }
            let active = current.as_deref() == Some(style.name.as_str());
            (style.name, about, active)
        }))
        .collect();
    let width = rows
        .iter()
        .map(|(name, ..)| name.chars().count())
        .max()
        .unwrap_or(0);
    for (name, about, active) in rows {
        let mark = if active {
            render::blue("●")
        } else {
            " ".into()
        };
        render::line(&format!(
            "{mark} {}  {}",
            render::bold(&format!("{name:<width$}")),
            render::dim(&about)
        ));
    }
    render::line(&render::dim(&format!(
        "/style <name> to switch. Write your own in {}, one .md file each.",
        styles::dir().display()
    )));
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

/// The MCP servers and how each is doing, or reconnect, enable, disable,
/// sign in to or out of or read the log of one, or read mcp.json again.
fn mcp_servers(runner: &mut Runner, rest: &str) {
    use letronna_core::mcp;
    let (verb, name) = rest.split_once(' ').unwrap_or((rest, ""));
    let name = name.trim();
    render::blank();
    if !verb.is_empty() && verb != "reload" && name.is_empty() {
        let which = format!("Which server? /mcp {verb} <name>");
        render::said(&render::blue("●"), &render::bold(&which));
        return;
    }
    let done = match verb {
        "" => Ok(()),
        "log" => return mcp_log(name),
        "reload" => mcp::reload().map(|done| {
            render::line(&render::dim(&format!("Reloaded mcp.json: {done}")));
        }),
        "reconnect" => mcp::reconnect(name),
        "enable" => mcp::set_enabled(name, true),
        "disable" => mcp::set_enabled(name, false),
        // The pasted address carries a code, so it is typed as a secret.
        "login" => crate::mcp::login(name, &mut |question| runner.bar.ask(question, true))
            .map(|()| render::line(&render::dim(&format!("Signed in to {name}.")))),
        "logout" => crate::mcp::logout(name),
        _ => Err(format!(
            "/mcp {verb} is not one of reconnect, enable, disable, login, logout, log or reload."
        )),
    };
    if let Err(error) = done {
        render::line(&render::red(&error));
        return;
    }
    // One just started gets as long to answer as a question asked once waits.
    if matches!(verb, "reconnect" | "enable" | "reload" | "login" | "logout") {
        mcp::ready(crate::MCP_WAIT);
    }
    if let Some(error) = mcp::unreadable() {
        render::line(&render::red(&error));
    }
    let servers = mcp::status();
    if servers.is_empty() {
        render::said(&render::blue("●"), &render::bold("No MCP servers"));
        render::line(&render::dim("Add one with `letronna mcp add` in a shell."));
        return;
    }
    render::said(&render::blue("●"), &render::bold("MCP servers"));
    for server in servers.iter().filter(|s| name.is_empty() || s.name == name) {
        render::line(&format!(
            "{}  {}  {}",
            render::bold(&server.name),
            render::dim(server.transport),
            crate::mcp::health(server)
        ));
    }
    render::line(&render::dim(
        "/mcp reconnect|enable|disable|login|logout|log <name>, or /mcp reload after editing mcp.json",
    ));
}

/// The newest of what one server printed on stderr.
fn mcp_log(name: &str) {
    let found = letronna_core::mcp::status()
        .into_iter()
        .find(|server| server.name == name);
    let Some(server) = found else {
        render::line(&render::red(&format!("There is no {name} in mcp.json.")));
        return;
    };
    if server.log.trim().is_empty() {
        let none = match server.transport {
            "http" => format!("{name} runs elsewhere, so it has no log here"),
            _ => format!("{name} has printed nothing"),
        };
        render::said(&render::blue("●"), &render::bold(&none));
        return;
    }
    let what = format!("What {name} printed, newest last");
    render::said(&render::blue("●"), &render::bold(&what));
    for line in server.log.lines() {
        render::line(&render::dim(line));
    }
}

/// The skills: list them, use one, switch one on or off, or add some.
///
/// Skills are written in the window and were unreachable from here, which
/// made them a feature of one half of the same program.
fn skills(runner: &mut Runner, rest: &str, workspace: Option<&std::path::Path>) {
    render::blank();
    // `/skills add <source>` and `/skills on|off <name>`; anything else names
    // a skill to use.
    let (verb, target) = rest
        .split_once(' ')
        .map_or((rest, ""), |(verb, target)| (verb, target.trim()));
    if verb == "add" {
        add_skills(runner, target);
        return;
    }
    // An old skills.json that will not move across is said, and the skills
    // already in folders are listed all the same.
    if let Err(why) = letronna_core::skills::migrate() {
        render::line(&render::red(&why));
    }
    let saved = match letronna_core::skills::list(workspace) {
        Ok(saved) => saved,
        Err(why) => {
            render::line(&render::red(&why));
            return;
        }
    };
    if matches!(verb, "on" | "off") {
        switch_skill(&saved, target, verb == "on");
        return;
    }
    if saved.is_empty() {
        render::said(&render::blue("●"), &render::bold("No skills yet"));
        render::line(&render::dim(
            "/skills add <owner/repo, link or path> to add some",
        ));
        return;
    }

    // Named, it goes to the model as the next thing said.
    if !rest.is_empty() {
        match pick_skill(&saved, rest) {
            Ok(skill) => {
                render::said(&render::blue("●"), &render::bold(&skill.name));
                runner.bar.say(&skill.body);
            }
            Err(why) => render::line(&render::dim(&why)),
        }
        return;
    }

    render::said(&render::blue("●"), &render::bold("Skills"));
    for skill in &saved {
        let state = if skill.enabled {
            render::green("on ")
        } else {
            render::dim("off")
        };
        let source = match skill.source {
            Source::User if skill.learned => "learned",
            Source::User => "yours",
            Source::Project => "project",
            Source::Plugin => "plugin",
        };
        // A line each: descriptions are written for the model, and run long.
        let about = format!("{source} · {}", skill.description);
        let room = render::width().saturating_sub(skill.name.chars().count() + 12);
        let about = if about.chars().count() > room {
            let cut: String = about.chars().take(room.saturating_sub(1)).collect();
            format!("{cut}…")
        } else {
            about
        };
        render::line(&format!(
            "{state}  {}  {}",
            render::bold(&skill.name),
            render::dim(&about)
        ));
    }
    render::line(&render::dim(
        "/skills <name> to use one · /skills on|off <name> · /skills add <owner/repo, link or path>",
    ));
}

/// The skill called `wanted`, or else the one whose name contains it. When
/// several do, none is guessed at: switching off the wrong one is a mistake
/// nobody sees.
fn pick_skill<'a>(saved: &'a [Skill], wanted: &str) -> Result<&'a Skill, String> {
    let lower = wanted.to_lowercase();
    if let Some(skill) = saved.iter().find(|s| s.name.to_lowercase() == lower) {
        return Ok(skill);
    }
    let close: Vec<&Skill> = saved
        .iter()
        .filter(|s| s.name.to_lowercase().contains(&lower))
        .collect();
    match close.as_slice() {
        [one] => Ok(one),
        [] => Err(format!("No skill matching \"{wanted}\".")),
        several => Err(format!(
            "Which one? {}",
            several
                .iter()
                .map(|s| s.name.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        )),
    }
}

/// Switch a skill on or off: only the ones on are offered to the model.
fn switch_skill(saved: &[Skill], wanted: &str, on: bool) {
    if wanted.is_empty() {
        let verb = if on { "on" } else { "off" };
        render::said(
            &render::blue("●"),
            &render::bold(&format!("Which one? /skills {verb} <name>")),
        );
        return;
    }
    let skill = match pick_skill(saved, wanted) {
        Ok(skill) => skill,
        Err(why) => {
            render::line(&render::dim(&why));
            return;
        }
    };
    match letronna_core::skills::set_enabled(&skill.name, on) {
        Ok(()) => render::said(
            &render::blue("●"),
            &render::bold(&format!(
                "{} is {}",
                skill.name,
                if on {
                    "on: the model can load it"
                } else {
                    "off: the model no longer sees it"
                }
            )),
        ),
        Err(why) => render::line(&render::red(&why)),
    }
}

/// Add skills from GitHub, a link or a path, and say what arrived, what it
/// replaced and what was left out.
fn add_skills(runner: &mut Runner, source: &str) {
    if source.is_empty() {
        render::said(
            &render::blue("●"),
            &render::bold("Add from where? /skills add <owner/repo, link or path>"),
        );
        return;
    }
    let from = source.to_owned();
    match runner.bar.busy("adding skills", move || {
        letronna_core::skills::install(&from)
    }) {
        Ok(installed) => {
            let mut done = Vec::new();
            if !installed.added.is_empty() {
                done.push(format!("Added {}", installed.added.join(", ")));
            }
            if !installed.updated.is_empty() {
                let names: Vec<&str> = installed
                    .updated
                    .iter()
                    .map(|(name, _)| name.as_str())
                    .collect();
                done.push(format!("Updated {}", names.join(", ")));
            }
            render::said(&render::blue("●"), &render::bold(&done.join("; ")));
            for (name, kept) in &installed.updated {
                render::line(&render::dim(&format!(
                    "The earlier {name} is kept in {}",
                    kept.display()
                )));
            }
            for why in &installed.refused {
                render::line(&render::amber(why));
            }
            render::line(&render::dim(
                "New skills start switched on, and an updated one keeps its switch. \
                 /skills off <name> to switch one off.",
            ));
        }
        Err(why) => render::line(&render::red(&why)),
    }
}

/// The plugins: list them and what each brings, or add, switch, allow,
/// update or remove one, or look through the marketplaces.
fn plugins(runner: &mut Runner, rest: &str) {
    use letronna_core::plugins;
    render::blank();
    let (verb, target) = rest
        .split_once(' ')
        .map_or((rest, ""), |(verb, target)| (verb, target.trim()));
    let needs_name = matches!(verb, "on" | "off" | "allow" | "remove" | "add");
    if needs_name && target.is_empty() {
        let what = if verb == "add" {
            "<git url or owner/repo>"
        } else {
            "<name>"
        };
        render::said(
            &render::blue("●"),
            &render::bold(&format!("Which one? /plugins {verb} {what}")),
        );
        return;
    }
    let done = match verb {
        "" | "list" => Ok(()),
        "market" => return plugin_market(runner, target),
        "add" => {
            let from = target.to_owned();
            match runner
                .bar
                .busy("cloning the plugin", move || plugins::add(&from))
            {
                Ok(plugin) => {
                    render::said(
                        &render::blue("●"),
                        &render::bold(&format!("Added {}", plugin.name)),
                    );
                    describe_plugin(&plugin);
                    return;
                }
                Err(why) => Err(why),
            }
        }
        "on" | "off" => plugins::set_enabled(target, verb == "on"),
        "allow" => plugins::allow(target),
        "remove" => plugins::remove(target).map(|kept| {
            render::line(&render::dim(&format!(
                "Removed {target}; it is kept in {}",
                kept.display()
            )));
        }),
        "update" => return update_plugins(runner, target),
        _ => Err(format!(
            "/plugins {verb} is not one of add, on, off, allow, update, remove or market."
        )),
    };
    if let Err(why) = done {
        render::line(&render::red(&why));
        return;
    }
    let installed = match plugins::list() {
        Ok(installed) => installed,
        Err(why) => {
            render::line(&render::red(&why));
            return;
        }
    };
    if installed.is_empty() {
        render::said(&render::blue("●"), &render::bold("No plugins yet"));
        render::line(&render::dim(
            "/plugins add <git url or owner/repo>, or /plugins market to see what is listed",
        ));
        return;
    }
    render::said(&render::blue("●"), &render::bold("Plugins"));
    for plugin in &installed {
        let state = if plugin.enabled {
            render::green("on ")
        } else {
            render::dim("off")
        };
        let version = match plugin.version.as_str() {
            "" => String::new(),
            version => format!(" {version}"),
        };
        render::line(&format!(
            "{state}  {}{}  {}",
            render::bold(&plugin.name),
            render::dim(&version),
            render::dim(&plugin.description)
        ));
        describe_plugin(plugin);
    }
    render::line(&render::dim(
        "/plugins add <url|owner/repo> · on|off|allow|update|remove <name> · market",
    ));
}

/// What a plugin brings, what of it is left out, and whether its servers
/// wait for the user's say.
fn describe_plugin(plugin: &letronna_core::plugins::Plugin) {
    if let Some(why) = &plugin.broken {
        render::line(&render::red(&format!("     {why}")));
        return;
    }
    let lists = [
        ("skills", &plugin.skills),
        ("commands", &plugin.commands),
        ("styles", &plugin.styles),
    ];
    for (what, names) in lists {
        if !names.is_empty() {
            render::line(&render::dim(&format!("     {what}: {}", names.join(", "))));
        }
    }
    for (name, target) in &plugin.servers {
        render::line(&render::dim(&format!("     MCP server {name}: {target}")));
    }
    for why in &plugin.clashes {
        render::line(&render::amber(&format!("     {why}")));
    }
    if !plugin.allowed {
        render::line(&render::amber(&format!(
            "     Its MCP servers run programs on this computer and wait for your say: \
             /plugins allow {} to start them.",
            plugin.name
        )));
    }
}

/// Update one plugin, or every one, from where each came.
fn update_plugins(runner: &mut Runner, name: &str) {
    use letronna_core::plugins;
    let names: Vec<String> = match name {
        "" => match plugins::list() {
            Ok(installed) => installed.into_iter().map(|p| p.name).collect(),
            Err(why) => {
                render::line(&render::red(&why));
                return;
            }
        },
        name => vec![name.to_owned()],
    };
    if names.is_empty() {
        render::said(&render::blue("●"), &render::bold("No plugins to update"));
        return;
    }
    for name in names {
        let which = name.clone();
        let done = runner
            .bar
            .busy(&format!("updating {name}"), move || plugins::update(&which));
        match done {
            Ok(None) => render::line(&render::dim(&format!("{name} is up to date"))),
            Ok(Some(kept)) => render::line(&format!(
                "{} {}",
                render::bold(&format!("Updated {name};")),
                render::dim(&format!("the earlier copy is kept in {}", kept.display()))
            )),
            Err(why) => render::line(&render::red(&why)),
        }
    }
}

/// The marketplaces and what each lists, or add or take away one.
fn plugin_market(runner: &mut Runner, rest: &str) {
    use letronna_core::plugins;
    let (verb, url) = rest
        .split_once(' ')
        .map_or((rest, ""), |(verb, url)| (verb, url.trim()));
    let changed = match verb {
        "add" => Some(plugins::add_market(url)),
        "remove" => Some(plugins::remove_market(url)),
        _ => None,
    };
    if let Some(Err(why)) = changed {
        render::line(&render::red(&why));
        return;
    }
    let markets = match runner.bar.busy("reading the marketplaces", plugins::market) {
        Ok(markets) => markets,
        Err(why) => {
            render::line(&render::red(&why));
            return;
        }
    };
    if markets.is_empty() {
        render::said(&render::blue("●"), &render::bold("No marketplaces yet"));
        render::line(&render::dim(
            "/plugins market add <https link to a JSON index of plugins>",
        ));
        return;
    }
    let installed: Vec<String> = plugins::list()
        .unwrap_or_default()
        .into_iter()
        .map(|p| p.name)
        .collect();
    for market in &markets {
        render::said(&render::blue("●"), &render::bold(&market.url));
        match &market.listings {
            Ok(listings) if listings.is_empty() => {
                render::line(&render::dim("It lists no plugins."));
            }
            Ok(listings) => {
                for listing in listings {
                    let mark = if installed.contains(&listing.name) {
                        render::green("installed")
                    } else {
                        render::dim(&listing.repo)
                    };
                    render::line(&format!(
                        "{}  {}  {}",
                        render::bold(&listing.name),
                        mark,
                        render::dim(&listing.description)
                    ));
                }
            }
            Err(why) => render::line(&render::red(why)),
        }
    }
    render::line(&render::dim(
        "/plugins add <repo> to install one · /plugins market add|remove <url>",
    ));
}

/// Sessions offered at once. Older ones are still on disk and still open in
/// the window; this is the shortlist you would actually pick from.
const SESSION_CHOICES: usize = 9;

/// How a session reads in the `/resume` list: its title, project folder and
/// length.
fn choice_label(conversation: &store::Conversation) -> String {
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
    // Listed sessions are not read yet, so their messages are empty and the
    // count comes from the listing.
    format!(
        "{title}  ·  {folder}  ·  {} messages",
        conversation.length()
    )
}

/// Pick up a session from before. The same files the window uses, so a
/// conversation started there can be carried on here. With a `term`, only
/// sessions whose title holds it are offered, or failing that, those where
/// it was said.
pub fn resume(
    runner: &mut Runner,
    session: &mut usize,
    workspace: Option<&std::path::Path>,
    term: &str,
) {
    // Newest first, skipping the one already open and any that never got a
    // word said in them.
    let lower = term.to_lowercase();
    let mut recent: Vec<usize> = (0..runner.store.conversations.len())
        .filter(|i| *i != *session && runner.store.conversations[*i].length() > 0)
        .filter(|i| {
            let title = &runner.store.conversations[*i].title;
            title.to_lowercase().contains(&lower)
        })
        .collect();
    recent.sort_by_key(|i| std::cmp::Reverse(runner.store.conversations[*i].updated));
    recent.truncate(SESSION_CHOICES);

    if recent.is_empty() && !term.is_empty() {
        resume_said(runner, session, workspace, term);
        return;
    }
    if recent.is_empty() {
        render::blank();
        render::line(&render::dim("No earlier sessions to pick up."));
        return;
    }

    let labels: Vec<String> = recent
        .iter()
        .map(|i| choice_label(&runner.store.conversations[*i]))
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
    pick_up(runner, session, workspace, index);
}

/// Sessions where `term` was said, for `/resume <term>` when no title holds
/// it: one line each, with a little of what was said, the best match first.
fn resume_said(
    runner: &mut Runner,
    session: &mut usize,
    workspace: Option<&std::path::Path>,
    term: &str,
) {
    let within = store::Within {
        exclude: runner.store.conversations[*session].id,
        ..Default::default()
    };
    // A session gives up to three hits; picking one opens the session, so
    // it is listed once, by its best.
    let mut hits = store::search(term, &within, SESSION_CHOICES * 3);
    let mut listed: Vec<u64> = Vec::new();
    hits.retain(|hit| {
        let first = !listed.contains(&hit.id);
        listed.push(hit.id);
        first
    });
    hits.truncate(SESSION_CHOICES);
    if hits.is_empty() {
        render::blank();
        render::line(&render::dim(&format!(
            "No earlier session has \"{term}\" in its title or what was said."
        )));
        return;
    }

    let room = render::width().saturating_sub(PICKER_EDGES);
    let labels: Vec<String> = hits
        .iter()
        .map(|hit| {
            let title = match hit.title.trim() {
                "" => "untitled",
                title => title,
            };
            // A match in the title has only the title to show.
            let label = match hit.ordinal {
                Some(_) => format!("{title}  ·  {}", from_match(&hit.snippet, term)),
                None => title.to_owned(),
            };
            // What was said can hold an escape a tool read from the web,
            // which printed raw would restyle or move about the terminal.
            render::fit(&label, room)
                .chars()
                .filter(|c| !c.is_control())
                .collect()
        })
        .collect();

    render::blank();
    render::said(
        &render::blue("●"),
        &render::bold("Said in earlier sessions"),
    );
    let mut options: Vec<&str> = labels.iter().map(String::as_str).collect();
    options.push("Stay where I am");
    let Some(picked) = runner.bar.choose("Which one?", Vec::new(), &options) else {
        return;
    };
    let Some(hit) = hits.get(picked) else {
        render::line(&render::dim("Stayed put."));
        return;
    };
    // One begun in the window since this terminal started is not listed
    // here yet, so it is read from its file.
    let index = match runner
        .store
        .conversations
        .iter()
        .position(|c| c.id == hit.id)
    {
        Some(index) => index,
        None => {
            let Some(conversation) = store::saved(hit.id) else {
                render::line(&render::red("That session is no longer on disk."));
                return;
            };
            runner.store.conversations.push(conversation);
            runner.store.conversations.len() - 1
        }
    };
    pick_up(runner, session, workspace, index);
}

/// Columns a picker line loses to the box around it and its number: two
/// margins, two borders with their spaces, and "› 10 ".
const PICKER_EDGES: usize = 13;
/// Characters of a snippet kept before its first match, so the match is on
/// screen after the session's title.
const SNIPPET_LEAD: usize = 20;

/// `snippet` from a little before the first of `query`'s words it holds,
/// ignoring case, so the match is not cut off the end of a picker line.
fn from_match(snippet: &str, query: &str) -> String {
    let lower = snippet.to_lowercase();
    let first = query
        .split(|c: char| c.is_whitespace() || c == '"')
        .map(|word| word.trim_matches(|c: char| c.is_ascii_punctuation()))
        .filter(|word| !word.is_empty())
        .filter_map(|word| lower.find(&word.to_lowercase()))
        .min();
    // Counted in characters, as core cuts its snippets.
    let skip = first.map_or(0, |at| {
        lower[..at].chars().count().saturating_sub(SNIPPET_LEAD)
    });
    if skip == 0 {
        return snippet.to_owned();
    }
    let rest: String = snippet.chars().skip(skip).collect();
    format!("…{}", rest.trim_start())
}

/// Make session `index` the open one and show what was said in it.
fn pick_up(
    runner: &mut Runner,
    session: &mut usize,
    workspace: Option<&std::path::Path>,
    index: usize,
) {
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

/// What changed in the project since its last commit, or with `session`
/// only in the files this session's tools wrote: git's summary, then the
/// patch in colour.
fn working_diff(
    runner: &mut Runner,
    session: usize,
    workspace: Option<&std::path::Path>,
    rest: &str,
) {
    render::blank();
    let conversation = &runner.store.conversations[session];
    let Some(folder) = conversation
        .workspace
        .clone()
        .or_else(|| workspace.map(Into::into))
    else {
        render::line(&render::dim("No project folder, so nothing to compare."));
        return;
    };
    let only = match rest {
        "" => None,
        "session" => Some(history::written(&conversation.messages, &folder)),
        _ => {
            render::line(&render::red(
                "Usage: /diff, or /diff session for only the files this session changed.",
            ));
            return;
        }
    };
    if only.as_ref().is_some_and(Vec::is_empty) {
        render::line(&render::dim("This session has not changed any files here."));
        return;
    }
    let diff = runner.bar.busy("reading the changes", move || {
        letronna_core::git::working_diff(&folder, only.as_deref())
    });
    let diff = match diff {
        Ok(diff) if diff.is_empty() => {
            render::line(&render::dim("Nothing has changed since the last commit."));
            return;
        }
        Ok(diff) => diff,
        Err(why) => {
            render::line(&render::dim(&why));
            return;
        }
    };
    let width = render::width().saturating_sub(render::PAD.len()).max(20);
    for line in diff.stat.lines() {
        render::line(&render::clip(line, width));
    }
    render::blank();
    for line in diff.patch.lines() {
        render::line(&crate::diff::patch_line(line, width));
    }
}

/// Sessions from Claude Code and Codex offered at once.
const IMPORT_CHOICES: usize = 9;

/// How a candidate reads in the `/import` list: its title, the tool it came
/// from and its project folder.
fn import_label(candidate: &letronna_core::import::Candidate) -> String {
    let title = match candidate.title.trim() {
        "" => "untitled",
        title => title,
    };
    let folder = candidate
        .workspace
        .as_ref()
        .and_then(|p| p.file_name())
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "no project".into());
    format!("{title}  ·  {}  ·  {folder}", candidate.tool.label())
}

/// Bring in a session from Claude Code or Codex as a session of this app's
/// own. Named or numbered with `term`, the matching one is imported without
/// asking which; bare, the newest from both are offered to pick from.
fn import(
    runner: &mut Runner,
    session: &mut usize,
    workspace: Option<&std::path::Path>,
    term: &str,
) {
    if !term.is_empty() {
        let path = std::path::PathBuf::from(term);
        let candidate = letronna_core::import::candidate_at(&path).or_else(|| {
            letronna_core::import::list(usize::MAX)
                .into_iter()
                .find(|c| c.session_id == term)
        });
        let Some(candidate) = candidate else {
            render::blank();
            render::line(&render::red(&format!(
                "No Claude Code or Codex session matches \"{term}\"."
            )));
            return;
        };
        import_one(runner, session, workspace, &candidate);
        return;
    }

    let candidates = letronna_core::import::list(IMPORT_CHOICES);
    if candidates.is_empty() {
        render::blank();
        render::line(&render::dim(
            "No Claude Code or Codex sessions found to import.",
        ));
        return;
    }

    let labels: Vec<String> = candidates.iter().map(import_label).collect();
    render::blank();
    render::said(
        &render::blue("●"),
        &render::bold("Continue from Claude Code or Codex"),
    );
    let mut options: Vec<&str> = labels.iter().map(String::as_str).collect();
    options.push("Never mind");
    let Some(picked) = runner.bar.choose("Which one?", Vec::new(), &options) else {
        return;
    };
    let Some(candidate) = candidates.get(picked) else {
        render::line(&render::dim("Stayed put."));
        return;
    };
    import_one(runner, session, workspace, candidate);
}

/// Import `candidate`, or open the session it was already imported into.
fn import_one(
    runner: &mut Runner,
    session: &mut usize,
    workspace: Option<&std::path::Path>,
    candidate: &letronna_core::import::Candidate,
) {
    if let Some(index) = runner
        .store
        .find_by_import(candidate.tool, &candidate.session_id)
    {
        render::blank();
        render::line(&render::dim("Already imported; opening it."));
        pick_up(runner, session, workspace, index);
        return;
    }
    let imported = match letronna_core::import::import(candidate) {
        Ok(imported) => imported,
        Err(error) => {
            render::blank();
            render::line(&render::red(&error));
            return;
        }
    };
    let skipped = imported.skipped;
    let id = runner.store.reserve_id();
    runner
        .store
        .conversations
        .push(letronna_core::import::into_conversation(imported, id));
    let index = runner.store.conversations.len() - 1;
    pick_up(runner, session, workspace, index);
    if skipped > 0 {
        render::line(&render::dim(&format!(
            "{skipped} part{} of the original session could not be imported.",
            if skipped == 1 { "" } else { "s" }
        )));
    }
}

/// Whether /undo's argument asks for the files too; `None` for one it does
/// not know, which must not run a plain undo that drops the files' history.
fn with_files(rest: &str) -> Option<bool> {
    match rest {
        "" => Some(false),
        "--files" => Some(true),
        _ => None,
    }
}

/// Take back the last exchange. With `files`, what its tools wrote goes
/// back too, once the list of those files is agreed to; without, the files
/// it wrote are named as staying.
fn undo(runner: &mut Runner, session: usize, workspace: Option<&std::path::Path>, files: bool) {
    let conversation = &runner.store.conversations[session];
    let id = conversation.id;
    let folder = conversation
        .workspace
        .clone()
        .or_else(|| workspace.map(std::path::Path::to_path_buf));
    let (plan, commands) = match last_prompt(&conversation.messages) {
        Some((at, _)) => {
            let dropped = &conversation.messages[at..];
            let plan = history::plan(id, dropped, folder.as_deref());
            (plan, history::ran_commands(dropped))
        }
        None => (Vec::new(), false),
    };
    if files && !plan.is_empty() {
        let mut detail = history::listing(&plan, folder.as_deref());
        if commands {
            detail.push(history::COMMANDS_NOTE.to_owned());
        }
        let options = [
            "Take it back and put the files back",
            "Leave everything as it is",
        ];
        let picked = runner
            .bar
            .choose("Put these files back too?", detail, &options);
        if picked != Some(0) {
            render::blank();
            render::line(&render::dim("Nothing taken back."));
            return;
        }
    }
    let Some(prompt) = take_back(runner, session, workspace) else {
        render::blank();
        render::line(&render::dim("Nothing to take back."));
        return;
    };
    runner.bar.say(&prompt);
    render::blank();
    render::line(&render::dim(
        "Took back the last exchange. Your message is back in the box.",
    ));
    if plan.is_empty() {
        return;
    }
    if !files {
        let names: Vec<String> = plan
            .iter()
            .map(|file| history::label(&file.path, folder.as_deref()))
            .collect();
        render::line(&render::dim(&format!(
            "The files it wrote stay as they are: {}.",
            names.join(", ")
        )));
        return;
    }
    // The question above went with the redraw, so what was left alone is
    // said again here.
    for line in history::restore(id, &plan, folder.as_deref()).lines() {
        render::line(&render::dim(&line));
    }
    if commands {
        render::line(&render::dim(history::COMMANDS_NOTE));
    }
}

/// Take the session back to just before the last thing you said, and the
/// screen with it. Returns what you said -- a command as it was typed -- or
/// `None` when nothing has been.
fn take_back(
    runner: &mut Runner,
    session: usize,
    workspace: Option<&std::path::Path>,
) -> Option<String> {
    let (at, prompt) = last_prompt(&runner.store.conversations[session].messages)?;
    let conversation = &mut runner.store.conversations[session];
    // Its pictures come back with its words, to go with them again.
    runner.images = std::mem::take(&mut conversation.messages[at].images);
    conversation.rewind(at);
    // The checklist is the one the last `todo` left standing.
    conversation.todos = store::todos_in(&conversation.messages);
    runner.bar.set_plan(&conversation.todos);
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
    let prompt = letronna_core::references::typed(&prompt);
    Some(
        letronna_core::init::typed(prompt)
            .unwrap_or(prompt)
            .to_owned(),
    )
}

/// Attach the picture at `raw` to the next message, saved as the window
/// saves one pasted there; with no path, say what is attached, and with
/// `clear`, drop it.
fn attach(runner: &mut Runner, raw: &str, workspace: Option<&std::path::Path>) {
    render::blank();
    if raw == "clear" {
        runner.images.clear();
        render::line(&render::dim("No pictures attached."));
        return;
    }
    if raw.is_empty() {
        let note = match runner.images.len() {
            0 => "Nothing attached. /image <path> attaches a picture to your next message.".into(),
            n => format!("{n} attached to your next message; /image clear drops them."),
        };
        render::line(&render::dim(&note));
        return;
    }
    let path = typed_path(raw, workspace);
    let name = path.file_stem().unwrap_or_default().to_string_lossy();
    let saved = std::fs::read(&path)
        .map_err(|e| format!("Cannot read {}: {e}", path.display()))
        .and_then(|bytes| letronna_core::media::attach(&name, &bytes));
    match saved {
        Ok(saved) => {
            runner.images.push(saved);
            render::line(&render::dim(&format!(
                "Attached {}; it goes with your next message.",
                path.display()
            )));
        }
        Err(error) => render::line(&render::red(&error)),
    }
}

/// Serve the HTML or SVG file `raw` names to the browser on this computer
/// and print where; `--open` first also opens it there.
fn preview(raw: &str, workspace: Option<&std::path::Path>) {
    render::blank();
    let (open, raw) = match raw.strip_prefix("--open") {
        Some(rest) if rest.is_empty() || rest.starts_with(' ') => (true, rest.trim()),
        _ => (false, raw),
    };
    if raw.is_empty() {
        render::line(&render::dim(
            "/preview <path> serves an HTML or SVG file to your browser; --open opens it too.",
        ));
        return;
    }
    match letronna_core::preview::start(&typed_path(raw, workspace)) {
        Ok(url) => {
            render::line(&url);
            render::line(&render::dim(&format!(
                "Only this computer can reach it, and it closes after {} minutes without a visit.",
                letronna_core::preview::IDLE.as_secs() / 60
            )));
            if open {
                open_in_browser(&url);
            }
        }
        Err(error) => render::line(&render::red(&error)),
    }
}

/// The file a typed path names: quoted, or with its spaces escaped as a
/// terminal pastes a file dragged onto it; `~` for home; and from the
/// project folder when relative.
fn typed_path(raw: &str, workspace: Option<&std::path::Path>) -> std::path::PathBuf {
    let quoted = ['"', '\''].iter().find_map(|&quote| {
        raw.strip_prefix(quote)
            .and_then(|inner| inner.strip_suffix(quote))
    });
    let raw = match quoted {
        Some(inner) => inner.to_owned(),
        // A backslash is the separator on Windows, not an escape.
        None if cfg!(windows) => raw.to_owned(),
        None => raw.replace("\\ ", " "),
    };
    let home = std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" });
    let path = match (raw.strip_prefix("~/"), home) {
        (Some(rest), Some(home)) => std::path::Path::new(&home).join(rest),
        _ => std::path::PathBuf::from(raw),
    };
    match workspace {
        Some(folder) => folder.join(path),
        None => path,
    }
}

/// Start, stop or show the open session's loop.
fn looping(runner: &Runner, session: usize, name: &str, rest: &str) {
    let id = runner.store.conversations[session].id;
    let Some(done) = letronna_core::wake::command(id, name, rest) else {
        return;
    };
    render::blank();
    match done {
        Ok(said) => render::line(&render::dim(&said)),
        Err(why) => render::line(&render::red(&why)),
    }
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
        id: runner.store.reserve_id(),
        title: title.clone(),
        messages: from.messages.clone(),
        updated: letronna_core::chat::now(),
        workspace: from.workspace.clone(),
        todos: from.todos.clone(),
        model: from.model.clone(),
        gateway: from.gateway.clone(),
        effort: from.effort.clone(),
        summary: from.summary.clone(),
        ..Default::default()
    };
    history::share(from.id, copy.id);
    runner.store.conversations.push(copy);
    *session = runner.store.conversations.len() - 1;
    runner.store.active = *session;
    runner.persist();
    runner.show_goal();
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

/// Change the model of `session` and of new sessions -- only the latter
/// before there is a session, in setup -- picking from what every set-up
/// gateway actually offers. `filter` narrows the list (by model or gateway
/// name), and an exact model name skips the asking entirely.
pub fn switch_model(runner: &mut Runner, session: Option<usize>, filter: &str) {
    // `/model image`, `/model video`: what makes those, not the chat model.
    if let Some((kind, label)) = media_kind(filter) {
        switch_media(runner, kind, label);
        return;
    }
    let keys = store::load_keys();
    let custom = runner.store.settings.custom_endpoint.clone();
    let settings = &runner.store.settings;
    let (gateway, model) = match session {
        Some(index) => runner.store.model(index),
        None => (settings.gateway.as_str(), settings.model.as_str()),
    };
    let current = format!("{model} · {gateway}");

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
        set_model(runner, session, gateway, model);
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
        Some((gateway, model)) => set_model(runner, session, gateway.clone(), model.clone()),
        None => render::line(&render::dim("Left as it was.")),
    }
}

/// A model and the gateway it is on, chosen together.
fn set_model(runner: &mut Runner, session: Option<usize>, gateway: String, model: String) {
    match session {
        Some(index) => runner.store.choose_model(index, &gateway, &model),
        None => {
            runner.store.settings.gateway = gateway.clone();
            runner.store.settings.model = model.clone();
        }
    }
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
    let keys = store::load_keys();
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
    let mut saved = Ok(Default::default());
    if what == "remove" {
        if let Err(error) = store::save_key(name, None) {
            render::line(&render::red(&format!("Could not remove {name}: {error}")));
            return;
        }
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
        saved = store::save_key(name, Some(given.trim()));
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
    if let Err(error) = saved {
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
    let mut saved = Ok(keys);
    match what {
        "remove" => {
            if let Err(error) = store::save_key(gateway::key_slot(name), None) {
                render::blank();
                render::line(&render::red(&format!("Could not remove {name}: {error}")));
                return;
            }
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
                saved = store::save_key(gateway::key_slot(name), Some(key.trim()));
            }
            render::blank();
            render::line(&render::dim(&format!(
                "{name} saved. /model now lists its models too."
            )));
        }
    }
    if let Err(error) = saved {
        render::line(&render::red(&format!("Could not save the key: {error}")));
    }
    runner.persist();
}

#[cfg(test)]
mod tests {
    use super::{
        agent_rows, choice_label, from_match, last_prompt, name_label, typed_path, with_files,
    };
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
    fn a_typed_or_dragged_path_names_its_file() {
        let project = std::path::Path::new("/work");
        let at = |raw: &str| typed_path(raw, Some(project));
        assert_eq!(at("shot.png"), project.join("shot.png"));
        assert_eq!(at("\"my shot.png\""), project.join("my shot.png"));
        assert_eq!(at("'my shot.png'"), project.join("my shot.png"));
        if !cfg!(windows) {
            assert_eq!(
                at("/tmp/my\\ shot.png"),
                std::path::Path::new("/tmp/my shot.png")
            );
            let home = std::env::var_os("HOME").unwrap();
            assert_eq!(at("~/a.png"), std::path::Path::new(&home).join("a.png"));
        }
        assert_eq!(typed_path("a.png", None), std::path::Path::new("a.png"));
    }

    #[test]
    fn undo_refuses_a_flag_it_does_not_know() {
        assert_eq!(with_files(""), Some(false));
        assert_eq!(with_files("--files"), Some(true));
        assert_eq!(with_files("--file"), None);
        assert_eq!(with_files("files"), None);
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

    #[test]
    fn a_hit_is_shown_from_just_before_its_match() {
        let snippet = format!("…{} the Signing key was rotated", "x".repeat(90));
        let shown = from_match(&snippet, "signing KEY");
        assert!(shown.starts_with('…'));
        assert!(shown.contains("the Signing key"));
        assert_eq!(
            shown.chars().count(),
            1 + 20 + "Signing key was rotated".len()
        );
        // Near the start already, or holding none of the words: as it is.
        assert_eq!(from_match("the signing key", "signing"), "the signing key");
        assert_eq!(
            from_match("nothing here", "\"signing key\""),
            "nothing here"
        );
    }

    #[test]
    fn a_listed_session_is_offered_with_the_messages_it_holds() {
        // As the terminal lists it at start: counted, none of it read yet.
        let listed = letronna_core::store::Conversation {
            title: "Fix the build".into(),
            workspace: Some("/work/letronna".into()),
            counted: 4,
            ..Default::default()
        };
        assert_eq!(
            choice_label(&listed),
            "Fix the build  ·  letronna  ·  4 messages"
        );
        // Once read, the messages themselves are the count.
        let read = letronna_core::store::Conversation {
            messages: vec![said("user", "one"), said("assistant", "two")],
            ..Default::default()
        };
        assert_eq!(
            choice_label(&read),
            "untitled  ·  no project  ·  2 messages"
        );
    }

    #[test]
    fn an_insights_name_is_one_short_line_with_no_escapes() {
        assert_eq!(name_label("read_file"), "read_file");
        assert_eq!(name_label("x\x1b[2J\x1b]0;hi\x07"), "x[2J]0;hi");
        assert_eq!(name_label("a\nb"), "a b");
        let long = name_label(&"t".repeat(10_000));
        assert_eq!(long.chars().count(), 40);
        assert!(long.ends_with('…'));
    }
}
