//! The slash commands: everything you can ask of Letronna itself rather
//! than of the model.

use letronna_core::agents::{Seen, took};
use letronna_core::budget::short;
use letronna_core::chat::{self, Connection, Message};
use letronna_core::fallback::Fallback;
use letronna_core::gate::Mode;
use letronna_core::inbox::{self, Notice, When};
use letronna_core::memory::{Saving, Scope};
use letronna_core::skills::{Skill, Source};
use letronna_core::slash::{self, Origin};
use letronna_core::tools::{Tool, ToolContext};
use letronna_core::workflow::tool::{LeftOpen, left_open};
use letronna_core::workflow::{self, RunState, RunView};
use letronna_core::{goal, history, store};
use serde_json::{Value, json};
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
    // Where the open session works -- its worktree, or the project of one
    // resumed from elsewhere -- for what a command reads or changes there;
    // the folder Letronna started in is for what is listed by project.
    let here_buf = (runner.store.conversations.get(*session)).and_then(|c| c.workspace.clone());
    let here = here_buf.as_deref().or(workspace);
    match line {
        "/exit" | "/quit" => Flow::Stop,
        "/help" => {
            help(here, rest, &runner.bar.moved_keys());
            Flow::Handled
        }
        "/keybindings" => {
            keybindings(runner);
            Flow::Handled
        }
        // A fresh start, as Claude Code's /clear is: a new session on a
        // clean screen. The one left stays in /resume; ctrl-l only clears.
        "/clear" => {
            *session = runner.open(false, workspace.map(|p| p.to_path_buf()));
            runner.persist();
            render::clear();
            banner(runner, &folder_name(here), *session);
            runner.bar.redraw();
            render::line(&render::dim(
                "New session; /resume brings back the one before.",
            ));
            Flow::Handled
        }
        "/status" => {
            status(runner, *session);
            Flow::Handled
        }
        "/terminal-setup" => {
            crate::terminal_setup::terminal_setup(runner);
            Flow::Handled
        }
        "/window" => {
            window(runner, *session);
            Flow::Handled
        }
        "/export" => {
            export(runner, *session, rest);
            Flow::Handled
        }
        "/new" => {
            // A session in a worktree of its own hands that on: the new one
            // gets one too, made from the same checkout, rather than working
            // in the checkout the worktree was there to spare.
            let tree = runner.store.conversations[*session].worktree.clone();
            let from = tree.as_ref().and_then(|tree| tree.checkout.clone());
            let folder = from.or_else(|| workspace.map(|p| p.to_path_buf()));
            *session = runner.open(false, folder);
            render::blank();
            if tree.is_some() {
                let conversation = &mut runner.store.conversations[*session];
                let name = format!("session-{}", conversation.id);
                match letronna_core::worktree::isolate(conversation, &name) {
                    Ok(()) => {
                        let branch = (conversation.worktree.as_ref())
                            .map_or(String::new(), |tree| tree.branch.clone());
                        render::line(&render::dim(&format!(
                            "New session, in a worktree of its own on branch {branch}."
                        )));
                    }
                    Err(error) => render::line(&render::red(&format!(
                        "New session, in the checkout: {error}"
                    ))),
                }
            } else {
                render::line(&render::dim("New session."));
            }
            runner.persist();
            Flow::Handled
        }
        "/plan" => {
            let plan = match rest {
                "on" => true,
                "off" => false,
                "" => runner.current_mode() != Mode::Plan,
                _ => {
                    render::line(&render::dim(
                        "/plan on or /plan off; /plan alone switches it.",
                    ));
                    return Flow::Handled;
                }
            };
            // Off goes back to the mode it was turned on from, not to Ask.
            let back = runner.bar.before_plan();
            runner.set_mode(if plan { Mode::Plan } else { back });
            render::blank();
            render::line(&render::dim(if plan {
                "Plan mode on: reads, searches and runs read-only commands, never edits your files."
            } else {
                "Plan mode off."
            }));
            Flow::Handled
        }
        "/mode" => {
            render::blank();
            // Bare, on a keyboard, the modes are offered with the one in use
            // marked; turned down, it is said as before.
            let picked = (rest.is_empty() && runner.bar.steered())
                .then(|| pick_mode(runner))
                .flatten();
            let rest = picked.unwrap_or(rest);
            if !rest.is_empty() {
                match Mode::parse(rest) {
                    Ok(mode) => runner.set_mode(mode),
                    Err(error) => {
                        render::line(&render::red(&error));
                        return Flow::Handled;
                    }
                }
            }
            let mode = runner.current_mode();
            render::line(&render::dim(&format!(
                "{}: {}.",
                mode.label(),
                mode.about()
            )));
            if rest.is_empty() && !runner.bar.steered() {
                let names = Mode::OFFERED.map(Mode::name).join(", ");
                render::line(&render::dim(&format!(
                    "Change it with /mode and one of {names}."
                )));
            }
            Flow::Handled
        }
        "/decisions" => {
            render::blank();
            let decisions = &runner.store.conversations[*session].decisions;
            for line in crate::session::decided(decisions, runner.current_mode()) {
                render::line(&render::dim(&line));
            }
            Flow::Handled
        }
        // How sessions are named, not the chat model.
        "/model" if rest.split_whitespace().next() == Some("names") => {
            name_model(runner, rest.strip_prefix("names").unwrap_or_default());
            Flow::Handled
        }
        // What summaries and checks run on, not the chat model.
        "/model" if rest.split_whitespace().next() == Some("side") => {
            side_model(runner, rest.strip_prefix("side").unwrap_or_default());
            Flow::Handled
        }
        "/model" => {
            switch_model(runner, Some(*session), rest);
            Flow::Handled
        }
        "/local" => {
            crate::local::slash(&mut runner.bar, rest, crate::local::watch);
            Flow::Handled
        }
        "/fallback" => {
            fallback(runner, rest);
            Flow::Handled
        }
        "/connect" | "/gateways" => {
            connect(runner);
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
            sign_out(runner);
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
        "/theme" => {
            theme(runner, rest);
            Flow::Handled
        }
        "/statusline" => {
            status_line(runner, *session, rest);
            Flow::Handled
        }
        "/thinking" => {
            let Some(on) = switched("/thinking", rest, runner.store.settings.thinking) else {
                return Flow::Handled;
            };
            runner.store.settings.thinking = on;
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
        "/vim" => {
            let Some(on) = switched("/vim", rest, runner.bar.vim_on()) else {
                return Flow::Handled;
            };
            runner.bar.set_vim(on);
            let kept = crate::vim::save(on);
            render::blank();
            render::line(&render::dim(if on {
                "Vim keys on: esc for normal mode, i to type again, /vim off to end them."
            } else {
                "Vim keys off."
            }));
            if let Err(why) = kept {
                render::line(&render::red(&why));
            }
            Flow::Handled
        }
        "/search" if rest == "services" => {
            search_services(runner);
            Flow::Handled
        }
        "/search" if rest.split_whitespace().next() == Some("letronna") => {
            letronna_search(runner, rest["letronna".len()..].trim());
            Flow::Handled
        }
        "/search" if rest.split_whitespace().next() == Some("keyless") => {
            let words = rest["keyless".len()..].trim();
            let now = runner.store.settings.keyless_search;
            if let Some(on) = switched("/search keyless", words, now) {
                keyless_search(runner, on);
            }
            Flow::Handled
        }
        "/search" => {
            let Some(on) = switched("/search", rest, runner.store.settings.search) else {
                return Flow::Handled;
            };
            runner.store.settings.search = on;
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
        "/tools" => {
            tools(runner, rest);
            Flow::Handled
        }
        "/tool-search" => {
            tool_search(runner, rest);
            Flow::Handled
        }
        "/notify" => {
            let Some(on) = switched("/notify", rest, runner.store.settings.notifications) else {
                return Flow::Handled;
            };
            runner.store.settings.notifications = on;
            runner.persist();
            render::blank();
            render::line(&render::dim(if on {
                "You are told when a reply ends or needs you while the terminal is in the background."
            } else {
                "Notifications off."
            }));
            Flow::Handled
        }
        "/coauthor" => {
            coauthor(runner, rest);
            Flow::Handled
        }
        "/voice" => {
            if let Some(on) = switched("/voice", rest, runner.store.settings.voice) {
                voice(runner, on);
            }
            Flow::Handled
        }
        "/verify" => {
            let Some(on) = switched("/verify", rest, runner.store.settings.verify_edits) else {
                return Flow::Handled;
            };
            runner.store.settings.verify_edits = on;
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
        "/format" => {
            format_edits(here, rest);
            Flow::Handled
        }
        "/memory-review" => {
            let Some(on) = switched("/memory-review", rest, runner.store.settings.memory_review)
            else {
                return Flow::Handled;
            };
            runner.store.settings.memory_review = on;
            runner.persist();
            render::blank();
            render::line(&render::dim(&if on {
                format!(
                    "Every {} of your messages, Letronna looks back over the session and adds what is worth remembering to memory. Each look is a model call on your account.",
                    letronna_core::review::EVERY
                )
            } else {
                "Memory reviews off.".into()
            }));
            // Otherwise each save is the user's to ask for or say yes to.
            let saving = runner.store.memory_saving(*session);
            if on && saving != Saving::Auto {
                render::line(&render::dim(&format!(
                    "They run only while memory is saved Automatically; this chat saves {}. /remember changes that.",
                    saving.label().to_lowercase()
                )));
            }
            Flow::Handled
        }
        "/remember" => remember(runner, *session, rest),
        "/jobs" => {
            jobs(runner, rest);
            Flow::Handled
        }
        "/loop" | "/heartbeat" => {
            looping(runner, *session, line, rest);
            Flow::Handled
        }
        "/agents" => {
            let id = runner.store.conversations[*session].id;
            agents(runner, id, rest);
            Flow::Handled
        }
        "/workflows" => {
            workflows(runner, *session, rest);
            Flow::Handled
        }
        "/crew" => {
            crew(runner.store.conversations[*session].id, rest);
            Flow::Handled
        }
        "/skills" => {
            skills(runner, rest, here);
            Flow::Handled
        }
        "/permissions" => {
            crate::permissions::permissions(runner, rest, here);
            Flow::Handled
        }
        "/mcp" => match rest.split_once(char::is_whitespace).unwrap_or((rest, "")) {
            // A prompt by its name under /mcp, as the menu offers it: none of
            // what /mcp does to a server has `__` in it.
            (prompt, args) if prompt.contains("__") => {
                send_prompt(runner, &format!("/mcp__{prompt}"), args.trim())
            }
            _ => {
                mcp_servers(runner, rest);
                Flow::Handled
            }
        },
        "/plugins" => {
            plugins(runner, rest);
            Flow::Handled
        }
        "/wallet" => {
            wallet(runner, rest);
            Flow::Handled
        }
        "/memory" => {
            crate::memory::memory(runner, rest, here);
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
        "/rewind" | "/undo" => {
            if rewinds(rest) {
                rewind(runner, session, here);
            } else {
                render::blank();
                render::line(&render::red(
                    "Usage: /rewind, or /undo: either lists your messages to go back to.",
                ));
            }
            Flow::Handled
        }
        "/redo" => {
            redo(runner, *session, here);
            Flow::Handled
        }
        "/retry" => {
            match take_back(runner, *session, here) {
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
        "/background" => match crate::background::hand_over(runner, *session, rest) {
            // Said on the way out, on the screen the terminal is given back.
            Ok(_) => Flow::Stop,
            Err(why) => {
                render::blank();
                render::line(&render::dim(&why));
                Flow::Handled
            }
        },
        "/rename" => {
            rename(runner, *session, rest);
            Flow::Handled
        }
        "/compact" => {
            compact(runner, *session, rest);
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
        "/recap" => {
            render::blank();
            match runner.recap(*session) {
                Ok(recap) => {
                    let mut lines = recap.lines();
                    let first = lines.next().unwrap_or_default();
                    render::said(&render::dim("◇"), &render::dim(first));
                    lines.for_each(|line| render::line(&render::dim(line)));
                }
                Err(error) => render::line(&render::red(&error)),
            }
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
            attach(runner, rest, here);
            Flow::Handled
        }
        "/preview" => {
            preview(rest, here);
            Flow::Handled
        }
        "/where" => {
            render::blank();
            render::line(&format!(
                "{}  {}",
                render::dim("project"),
                here.map(|p| p.display().to_string())
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
        "/copy" => {
            copy_reply(runner, *session, rest);
            Flow::Handled
        }
        "/usage" => {
            usage(runner.store.conversations[*session].id);
            Flow::Handled
        }
        "/limits" => {
            limits(runner);
            Flow::Handled
        }
        "/context" => {
            context(runner, *session);
            Flow::Handled
        }
        "/retention" => {
            retention(runner, rest);
            Flow::Handled
        }
        "/insights" => {
            insights(rest);
            Flow::Handled
        }
        name if name.starts_with("/mcp__") => send_prompt(runner, name, rest),
        _ => {
            let conversation = (runner.store.conversations.get(*session)).map_or(0, |c| c.id);
            added(runner, whole, here, conversation)
        }
    }
}

/// `/window`: carry the open session on in the window.
fn window(runner: &mut Runner, session: usize) {
    render::blank();
    let conversation = &runner.store.conversations[session];
    if conversation.length() == 0 {
        render::line(&render::dim(
            "Nothing has been said here yet; the window starts its own with New session.",
        ));
        return;
    }
    let id = conversation.id;
    // On disk before the window reads it.
    runner.persist();
    match crate::open_in_window(id) {
        Ok(()) => render::line(&render::dim(&format!(
            "Opened this session in the window. Carry it on there or here, one at a \
             time; letronna --resume {id} brings it back to a terminal."
        ))),
        Err(why) => render::line(&render::red(&why)),
    }
}

/// Send the MCP server's prompt behind `command`, filled in with `rest`, or
/// say why not.
fn send_prompt(runner: &mut Runner, command: &str, rest: &str) -> Flow {
    match mcp_prompt(runner, command, rest) {
        Some(Ok(text)) => Flow::Send(text),
        Some(Err(error)) => {
            render::blank();
            render::line(&render::red(&error));
            Flow::Handled
        }
        None => Flow::Handled,
    }
}

/// What `/tool-search` was asked to do.
#[derive(Debug, PartialEq)]
enum ToolSearch {
    Show,
    /// Ask Jev one small question now, and say how it went.
    Check,
    /// Rank by Jev where the chat is hosted, Jev anywhere, or by names.
    Rank(&'static str),
    /// Ask Jev at this address; empty for the Letronna account.
    Address(String),
    /// Ask this Jev model; empty for the latest.
    Model(String),
    /// Ask for a Jev key of the user's own, or remove it.
    Key {
        remove: bool,
    },
}

/// What `/tool-search` followed by `rest` asks for, or why it cannot be done.
fn tool_search_asked(rest: &str) -> Result<ToolSearch, String> {
    let (what, value) = rest
        .trim()
        .split_once(char::is_whitespace)
        .map_or((rest.trim(), ""), |(what, value)| (what, value.trim()));
    Ok(match what {
        "" => ToolSearch::Show,
        "check" => ToolSearch::Check,
        "auto" => ToolSearch::Rank("auto"),
        "jev" => ToolSearch::Rank("jev"),
        "names" => ToolSearch::Rank("names"),
        "key" => ToolSearch::Key {
            remove: value == "remove",
        },
        "model" => ToolSearch::Model(value.to_owned()),
        "address" if value.is_empty() || value == "letronna" => ToolSearch::Address(String::new()),
        "address" => {
            let here = ["http://localhost", "http://127.0.0.1", "http://[::1]"];
            if !value.starts_with("https://") && !here.iter().any(|h| value.starts_with(h)) {
                return Err(format!(
                    "{value} is not used: a key goes only to an https address, or to one on this computer."
                ));
            }
            ToolSearch::Address(value.to_owned())
        }
        _ => {
            return Err(
                "/tool-search auto, jev or names · check · key (or key remove) · address <url> or letronna · model <name>"
                    .into(),
            );
        }
    })
}

/// The ways find_tools can rank, as `/tool-search` offers them: the setting,
/// its name and what it means.
const RANKINGS: [(&str, &str, &str); 3] = [
    (
        "auto",
        "Automatic",
        "Jev ranks them while the chat is on a hosted model; for a model on this computer, they are ranked here by their words",
    ),
    (
        "jev",
        "Always Jev",
        "Jev ranks every search, a model on this computer included",
    ),
    (
        "names",
        "Words only",
        "ranked here by their names and descriptions, and nothing is sent",
    ),
];

/// How the last search's tools were ranked, in a line.
fn last_ranking() -> String {
    use letronna_core::jev::Outcome;
    match letronna_core::jev::last() {
        None => "No tools searched for yet.".to_owned(),
        Some(Outcome::Jev(Some(took))) => {
            format!("Last search ranked by Jev in {:.1} s.", took.as_secs_f64())
        }
        Some(Outcome::Jev(None)) => "Last search ranked by Jev.".to_owned(),
        Some(Outcome::Words(why)) => format!("Last search ranked by words; Jev: {why}."),
        Some(Outcome::NotAsked(why)) => format!("Last search ranked by words, as {why}."),
    }
}

/// Where Jev is asked, as the settings and keys have it: "at TypeSafe",
/// "through your Letronna account" and the like.
fn jev_place(runner: &Runner, own_key: bool) -> String {
    match (runner.store.settings.jev_endpoint.trim(), own_key) {
        ("", true) => "at TypeSafe, with your own key".to_owned(),
        ("", false) if letronna_core::auth::account().is_none() => {
            "nowhere, as this computer is signed out and no Jev key is set".to_owned()
        }
        ("", false) => "through your Letronna account".to_owned(),
        (address, _) => format!("at {address}"),
    }
}

/// Show how find_tools ranks what it finds and where Jev is asked, and
/// offer to change it; or change it, or check Jev, as `rest` says.
fn tool_search(runner: &mut Runner, rest: &str) {
    render::blank();
    let mut asked = match tool_search_asked(rest) {
        Ok(asked) => asked,
        Err(error) => {
            render::line(&render::red(&error));
            return;
        }
    };
    let own_key = || {
        store::load_keys()
            .get(letronna_core::jev::KEY_SLOT)
            .is_some_and(|key| !key.is_empty())
    };
    if asked == ToolSearch::Show {
        let now = runner.store.settings.tool_search.clone();
        let rows = vec![
            format!("{} · {}", ranking(&now).1, ranking(&now).2),
            format!("Jev is asked {}.", jev_place(runner, own_key())),
            last_ranking(),
        ];
        let key = match own_key() {
            true => "Remove your Jev key",
            false => "Set a Jev key of your own",
        };
        let mut options: Vec<&str> = RANKINGS.iter().map(|(_, name, _)| *name).collect();
        options.extend(["Check Jev now", key, "Done"]);
        let Some(picked) = runner.bar.choose("Finding tools", rows, &options) else {
            return;
        };
        asked = match picked {
            at if at < RANKINGS.len() => ToolSearch::Rank(RANKINGS[at].0),
            at if at == RANKINGS.len() => ToolSearch::Check,
            at if at == RANKINGS.len() + 1 => ToolSearch::Key { remove: own_key() },
            _ => return,
        };
    }
    let settings = &mut runner.store.settings;
    match asked {
        ToolSearch::Show => {}
        ToolSearch::Check => {
            render::line(&render::dim("Asking Jev one small question…"));
            match letronna_core::jev::check() {
                Ok(took) => render::line(&render::green(&format!(
                    "Jev answered in {:.1} s.",
                    took.as_secs_f64()
                ))),
                Err(why) => {
                    render::line(&render::amber(&why));
                    render::line(&render::dim(
                        "Until it answers, find_tools ranks tools by their words.",
                    ));
                }
            }
            return;
        }
        ToolSearch::Rank(how) => settings.tool_search = how.to_owned(),
        ToolSearch::Address(url) => settings.jev_endpoint = url,
        ToolSearch::Model(model) => settings.jev_model = model,
        ToolSearch::Key { remove } => {
            let saved = if remove {
                store::save_key(letronna_core::jev::KEY_SLOT, None)
            } else {
                let given = runner.bar.ask("Jev key", true).unwrap_or_default();
                if given.trim().is_empty() {
                    return;
                }
                store::save_key(letronna_core::jev::KEY_SLOT, Some(given.trim()))
            };
            if let Err(error) = saved {
                render::line(&render::red(&format!(
                    "Could not save the Jev key: {error}"
                )));
                return;
            }
        }
    }
    runner.persist();
    let settings = &runner.store.settings;
    let (_, name, means) = ranking(&settings.tool_search);
    let model = Some(settings.jev_model.trim())
        .filter(|model| !model.is_empty())
        .unwrap_or("jev-latest");
    render::line(&render::dim(&format!(
        "find_tools ranks the tools left out of a request: {name}, {means}. Jev ({model}) \
         is asked {}, and is sent the search's words and the tools' names and \
         descriptions.",
        jev_place(runner, own_key())
    )));
    render::line(&render::dim(
        "/tool-search for the choices · check · auto, jev or names · key · address <url> or letronna · model <name>",
    ));
}

/// The ranking `setting` names, Automatic for any it does not.
fn ranking(setting: &str) -> (&'static str, &'static str, &'static str) {
    RANKINGS
        .iter()
        .copied()
        .find(|(value, _, _)| *value == setting)
        .unwrap_or(RANKINGS[0])
}

/// Show how long sessions are kept, or set it from `rest`.
fn retention(runner: &mut Runner, rest: &str) {
    render::blank();
    let days = match keep_days(rest) {
        Ok(Some(days)) => {
            runner.store.settings.keep_days = days;
            runner.persist();
            days
        }
        Ok(None) => runner.store.settings.keep_days,
        Err(error) => {
            render::line(&render::red(&error));
            return;
        }
    };
    render::line(&render::dim(&match days {
        0 => "Every session is kept. /retention 30 deletes those untouched for 30 days.".into(),
        days => format!(
            "Sessions untouched for {days} day{} are deleted when Letronna starts with no \
             other copy open, at most once a day; pinned ones are kept. /retention off keeps \
             them all.",
            if days == 1 { "" } else { "s" }
        ),
    }));
}

/// Show the side model, or set it from `rest`.
fn side_model(runner: &mut Runner, rest: &str) {
    render::blank();
    if let Some(model) = side_setting(rest) {
        runner.store.settings.side_model = model;
        runner.persist();
    }
    render::line(&render::dim(&match runner.store.settings.side_model.as_str() {
        "" => "Summaries, commit messages and goal checks use the session's model. /model side <model> names a cheaper one on the same gateway.".to_owned(),
        model => format!(
            "Summaries, commit messages and goal checks use {model}, on the session's gateway, or the session's model when the gateway turns it away. /model side off puts them back on the session's model."
        ),
    }));
}

/// Show how sessions are named, or set it from `rest`; from the keyboard
/// with nothing given, pick it from a list.
fn name_model(runner: &mut Runner, rest: &str) {
    render::blank();
    let given = match rest.trim() {
        "" if runner.bar.steered() => pick_naming(runner),
        rest => names_setting(rest),
    };
    if let Some((on, model)) = given {
        let settings = &mut runner.store.settings;
        settings.auto_title = on;
        if let Some(model) = model {
            settings.title_model = model;
        }
        runner.persist();
    }
    render::line(&render::dim(&naming_said(
        runner.store.settings.auto_title,
        &runner.store.settings.title_model,
    )));
}

/// What `/model names` was given: None to only show it; else whether a
/// model names sessions, and which, when one was said.
fn names_setting(rest: &str) -> Option<(bool, Option<String>)> {
    match rest.trim() {
        "" => None,
        "off" | "none" => Some((false, None)),
        "on" => Some((true, None)),
        "auto" => Some((true, Some(String::new()))),
        model => Some((true, Some(model.to_owned()))),
    }
}

/// How sessions are named, asked in a list: automatically, by the chat's
/// own model, by a model typed in, or not at all.
fn pick_naming(runner: &mut Runner) -> Option<(bool, Option<String>)> {
    let settings = &runner.store.settings;
    let now = match (settings.auto_title, settings.title_model.as_str()) {
        (false, _) => 3,
        (true, "") => 0,
        (true, "chat") => 1,
        (true, _) => 2,
    };
    let options = [
        "Automatically: Letronna's quick model on Letronna, else the side model or the chat's own",
        "With the chat's own model",
        "With a model you name",
        "Not at all: keep the first line of what was asked",
    ];
    let shown: Vec<String> = (options.iter().enumerate())
        .map(|(n, option)| match n == now {
            true => format!("{option} (in use)"),
            false => (*option).to_owned(),
        })
        .collect();
    let shown: Vec<&str> = shown.iter().map(String::as_str).collect();
    let detail = vec!["A model names a session once its first reply is in.".to_owned()];
    match runner
        .bar
        .choose("How are sessions named?", detail, &shown)?
    {
        0 => Some((true, Some(String::new()))),
        1 => Some((true, Some("chat".into()))),
        2 => {
            let model = runner.bar.ask("the model that names sessions", false)?;
            names_setting(&model).filter(|(on, _)| *on)
        }
        _ => Some((false, None)),
    }
}

/// How sessions are named, as `/model names` says it: naming `on`, by the
/// `model` Settings give.
fn naming_said(on: bool, model: &str) -> String {
    if !on {
        return "Sessions keep the first line of what was asked. /model names auto has a model \
                name them; /rename names one now."
            .into();
    }
    match model.trim() {
        "" => "A model names each session once its first reply is in: Letronna's quick model \
               for a chat on Letronna, checked by Jev, else the side model, else the chat's own. \
               A chat on another gateway is named there. /model names chat, <model> or off \
               changes that."
            .into(),
        "chat" => "The chat's own model names each session once its first reply is in. /model \
                   names auto, <model> or off changes that."
            .into(),
        model => format!(
            "{model} names each session once its first reply is in, checked by Jev, or the \
             chat's own model when it does not answer. /model names auto, chat or off changes \
             that."
        ),
    }
}

/// Name the session `rest`, or with nothing, have a model name it now.
fn rename(runner: &mut Runner, session: usize, rest: &str) {
    render::blank();
    let given = rest.trim();
    if !given.is_empty() {
        runner.store.conversations[session].title = given.to_owned();
        runner.persist();
        render::line(&render::dim(&format!("Named this session “{given}”.")));
        return;
    }
    let conversation = &runner.store.conversations[session];
    if conversation.messages.is_empty() {
        render::line(&render::dim(
            "Nothing has been said yet to name the session after; /rename <name> names it.",
        ));
        return;
    }
    let (connection, messages) = (runner.connection(session), conversation.messages.clone());
    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let stopped = stop.clone();
    let named = runner
        .bar
        .busy_cancellable("naming the session · esc to stop", stop, move || {
            let (sender, receiver) = std::sync::mpsc::channel();
            std::thread::spawn(move || {
                let _ = sender.send(letronna_core::title::name(&connection, &messages));
            });
            // A request cannot hear a stop, so it is left to end on its own
            // and what it gives goes unused.
            loop {
                match receiver.recv_timeout(std::time::Duration::from_millis(100)) {
                    Ok(named) => return named,
                    Err(std::sync::mpsc::RecvTimeoutError::Timeout)
                        if !stopped.load(Ordering::Relaxed) => {}
                    Err(_) => return Err("Stopped; the name stays as it was.".into()),
                }
            }
        });
    match named {
        Ok(name) => {
            runner.store.conversations[session].title = name.clone();
            runner.persist();
            render::line(&render::dim(&format!(
                "Named this session “{name}”; /rename <name> names it yourself."
            )));
        }
        Err(why) => render::line(&render::red(&why)),
    }
}

/// The side model `/model side` was given: None to only show it, empty for
/// the session's own.
fn side_setting(rest: &str) -> Option<String> {
    match rest.trim() {
        "" => None,
        "off" | "none" => Some(String::new()),
        model => Some(model.to_owned()),
    }
}

/// The days `/retention` was given: None to only show them, 0 to keep every
/// session.
fn keep_days(rest: &str) -> Result<Option<u32>, String> {
    match rest.trim() {
        "" => Ok(None),
        "off" | "never" | "forever" => Ok(Some(0)),
        days => days.parse().map(Some).map_err(|_| {
            format!("\"{days}\" is not a number of days. Try /retention 30, or /retention off.")
        }),
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
/// the box, to be read before it is sent. A saved workflow's runs instead,
/// and so does a plugin's action, in `conversation`. Unknown when none has
/// the name.
fn added(
    runner: &mut Runner,
    line: &str,
    workspace: Option<&std::path::Path>,
    conversation: u64,
) -> Flow {
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
    if command.origin == Origin::PluginAction {
        plugin_action(runner, &command.name, args, workspace, conversation);
        return Flow::Handled;
    }
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

/// Runs a plugin's action `name` with what was typed after it, with the
/// wheel turning: what it gives back is shown, or put in the box when it is
/// words for the user to send. Escape stops waiting for it.
fn plugin_action(
    runner: &mut Runner,
    name: &str,
    args: &str,
    workspace: Option<&std::path::Path>,
    conversation: u64,
) {
    use letronna_core::plugins::code::{self, Said};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::mpsc::RecvTimeoutError;
    let (name, args) = (name.to_owned(), args.to_owned());
    let workspace = workspace.map(std::path::Path::to_path_buf);
    let stop = std::sync::Arc::new(AtomicBool::new(false));
    let given_up = stop.clone();
    let label = format!("running /{name} · esc to cancel");
    let outcome = runner.bar.busy_cancellable(&label, stop, move || {
        let cancel = letronna_core::cancel::Cancel::default();
        let (done, answer) = std::sync::mpsc::channel();
        let asked = cancel.clone();
        std::thread::spawn(move || {
            let said = code::run_action(&name, &args, conversation, workspace.as_deref(), &asked);
            let _ = done.send(said);
        });
        loop {
            match answer.recv_timeout(std::time::Duration::from_millis(100)) {
                Ok(outcome) => return Some(outcome),
                Err(RecvTimeoutError::Disconnected) => return None,
                Err(RecvTimeoutError::Timeout) if given_up.load(Ordering::Relaxed) => {
                    cancel.set();
                    return None;
                }
                Err(RecvTimeoutError::Timeout) => {}
            }
        }
    });
    match outcome {
        Some(Ok(Said::Text(text))) => {
            for line in text.lines() {
                render::line(line);
            }
        }
        Some(Ok(Said::Compose(given))) => {
            let given = match runner.bar.draft().trim() {
                "" => given,
                draft => format!("{draft}\n{given}"),
            };
            runner.bar.say(&given);
        }
        Some(Err(error)) => render::line(&render::red(&error)),
        None => render::line(&render::dim("Stopped waiting for it.")),
    }
}

/// Offer the added commands in the `/` menu, read afresh, so a file made or
/// changed shows from the next command on. Skills go apart from the rest,
/// to be offered once their name is typed and under `/skills`.
pub fn offer_added(runner: &mut Runner, workspace: Option<&std::path::Path>) {
    let (skills, commands): (Vec<_>, Vec<_>) = listed(workspace)
        .into_iter()
        .filter(|c| c.hidden_by.is_none())
        .partition(|c| c.origin == Origin::Skill);
    let row = |c: slash::Command| {
        let blurb = match c.hint.as_str() {
            "" => c.description,
            hint => format!("{hint} · {}", c.description),
        };
        (format!("/{}", c.name), blurb)
    };
    // The ones switched off too, so a whole name typed under /skills is
    // never taken for a longer one that is on.
    let shelf = letronna_core::skills::list(workspace)
        .unwrap_or_default()
        .into_iter()
        .map(|skill| {
            let blurb = if skill.enabled {
                skill.description
            } else {
                format!("off · {}", skill.description)
            };
            (skill.name, blurb)
        })
        .collect();
    runner.bar.offer(bar::Added {
        commands: commands.into_iter().map(row).collect(),
        skills: skills.into_iter().map(row).collect(),
        shelf,
    });
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
        Origin::Plugin => "a plugin's",
        Origin::Project => "the project's",
        Origin::Skill => "a skill",
        Origin::Workflow => "a saved workflow",
        Origin::PluginAction => "a plugin's code",
    };
    format!("{from} · hidden, {winner} has the name")
}

/// Where an added command comes from, in a word.
fn from(origin: Origin) -> &'static str {
    match origin {
        // Only a name's winner is ever built in; the match must name it.
        Origin::BuiltIn => "built in",
        Origin::User => "yours",
        Origin::Plugin => "plugin",
        Origin::Project => "project",
        Origin::Skill => "skill",
        Origin::Workflow => "workflow",
        Origin::PluginAction => "plugin",
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

/// Copy a reply as it was written, Markdown and all: the latest, or with a
/// number the one that many back, as `/copy 2` takes the one before last.
fn copy_reply(runner: &mut Runner, session: usize, rest: &str) {
    let back = match rest.trim() {
        "" => 1,
        number => match number.parse::<usize>() {
            Ok(back) if back > 0 => back,
            _ => {
                render::line(&render::dim(
                    "/copy copies the last reply; /copy 2 the one before it.",
                ));
                return;
            }
        },
    };
    let messages = &runner.store.conversations[session].messages;
    let Some(reply) = nth_reply(messages, back) else {
        render::line(&render::dim(match back {
            1 => "There is no reply to copy yet.",
            _ => "There are not that many replies yet.",
        }));
        return;
    };
    let took = crate::screen::copy(reply);
    render::blank();
    let said = render::dim(&crate::bar::copied(reply, took));
    render::line(&match took {
        true => format!("{} {said}", render::green("✓")),
        false => said,
    });
}

/// The text of the reply `back` from the latest, the latest being 1: what
/// the model wrote for the user to read, not a step that only called tools.
fn nth_reply(messages: &[Message], back: usize) -> Option<&str> {
    (messages.iter().rev())
        .filter(|m| m.role == "assistant" && !m.content.trim().is_empty())
        .nth(back - 1)
        .map(|m| m.content.as_str())
}

/// Run what was typed after `!` in the session's folder, through the tool
/// the model runs commands with but without asking, since the user typed
/// it, and Esc stops it as it stops one of the model's. What it printed is
/// shown as a tool's output is, and goes to the model with the next prompt,
/// so it knows what was run and what came of it.
pub fn shell_line(runner: &mut Runner, session: usize, command: &str) {
    render::blank();
    if command.is_empty() {
        render::line(&render::dim(
            "! and a command runs it here, as in a shell: !git status",
        ));
        return;
    }
    let connection = runner.connection(session);
    let mut ctx = letronna_core::tools::ToolContext::of(&connection);
    ctx.approved = true;
    let (cancel, conversation) = (ctx.cancel.clone(), ctx.conversation);
    let args = serde_json::json!({ "command": command }).to_string();
    let handle = std::thread::spawn(move || letronna_core::tools::run("run_command", &args, &ctx));
    runner.bar.wait_line_about(&format!("! {command}"));
    runner.bar.working(true);
    while !handle.is_finished() {
        runner.bar.pump();
        if runner.bar.interrupted() {
            cancel.set();
        }
        let movable = letronna_core::tools::can_go_to_background(conversation);
        if runner.bar.background_asked(movable) {
            letronna_core::tools::to_background(conversation);
        }
    }
    runner.bar.working(false);
    runner.bar.wait_line(false);
    let ran =
        (handle.join()).unwrap_or_else(|_| Err("The command stopped without finishing.".into()));
    let (output, failed) = match ran {
        Ok(output) => (output, false),
        Err(error) => (error, true),
    };
    let first = output.lines().next().unwrap_or("done").to_owned();
    render::tool(
        "!",
        command,
        &letronna_core::tools::one_line(&first),
        failed,
    );
    let rest: Vec<&str> = output.lines().skip(1).collect();
    if !rest.is_empty() {
        render::detail(&rest.join("\n"), 12);
    }
    // Quiet: it goes in with the next prompt, rather than starting a turn.
    let id = runner.store.conversations[session].id;
    letronna_core::inbox::push(
        id,
        letronna_core::inbox::Notice {
            from: "the shell".to_owned(),
            when: letronna_core::inbox::When::Quiet,
            text: format!("The user ran `{command}` in the terminal:\n{output}"),
            files: Vec::new(),
        },
    );
}

/// A path as `/status` shows it, the home folder written `~`: the session
/// file under the data folder fits beside its label that way, where the
/// whole path would wrap.
fn shown_path(path: &std::path::Path, home: Option<&std::path::Path>) -> String {
    match home.and_then(|home| path.strip_prefix(home).ok()) {
        Some(inside) if inside.as_os_str().is_empty() => "~".to_owned(),
        Some(inside) => std::path::Path::new("~").join(inside).display().to_string(),
        None => path.display().to_string(),
    }
}

/// Where things stand, on one screen: the version, the session, its
/// folder and branch, the model, the mode, the account, how full the
/// context is, and the MCP servers.
fn status(runner: &mut Runner, session: usize) {
    let conversation = &runner.store.conversations[session];
    let (id, title) = (conversation.id, conversation.title.clone());
    let folder = conversation.workspace.clone();
    let filled = (conversation.messages.iter().rev())
        .find(|m| m.role == "assistant" && m.window > 0 && m.input_tokens() > 0)
        .map(|reply| (reply.input_tokens(), reply.window));
    let place = (folder.clone()).map(|folder| {
        runner.bar.busy("looking at the project", move || {
            letronna_core::git::status(&folder).map(|status| status.describe())
        })
    });
    let (gateway, model) = runner.store.model(session);
    let (gateway, model) = (gateway.to_owned(), model.to_owned());
    let effort = runner.store.effort(session).to_owned();
    let row = |label: &str, value: &str| {
        render::line(&format!("{}  {value}", render::dim(&format!("{label:<8}"))));
    };
    render::blank();
    row("version", env!("CARGO_PKG_VERSION"));
    let name = match letronna_core::tools::one_line(&title) {
        name if name.is_empty() => "untitled".to_owned(),
        name => name,
    };
    row("session", &format!("{name} · {id}"));
    let home = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(std::path::PathBuf::from);
    let file = store::sessions_dir().join(format!("{id}.jsonl"));
    row("file", &shown_path(&file, home.as_deref()));
    match (&folder, place) {
        (Some(folder), Some(Ok(place))) => row(
            "folder",
            &format!("{} · {place}", shown_path(folder, home.as_deref())),
        ),
        (Some(folder), _) => row("folder", &shown_path(folder, home.as_deref())),
        (None, _) => row("folder", "none"),
    }
    let thinking = match effort.as_str() {
        "" => String::new(),
        effort => format!(" · thinking {effort}"),
    };
    row("model", &format!("{model} on {gateway}{thinking}"));
    let mode = runner.current_mode();
    row("mode", &format!("{}: {}", mode.label(), mode.about()));
    row(
        "account",
        &match letronna_core::auth::account() {
            Some(account) => format!("signed in as {}", account.email),
            None => "not signed in · /login".to_owned(),
        },
    );
    if let Some((used, window)) = filled {
        row(
            "context",
            &format!(
                "{}% of {}",
                used.saturating_mul(100) / window.max(1),
                letronna_core::usage::count(window)
            ),
        );
    }
    let servers = letronna_core::mcp::status();
    if !servers.is_empty() {
        let up = (servers.iter())
            .filter(|server| server.state == letronna_core::mcp::State::Connected)
            .count();
        row(
            "mcp",
            &format!("{} servers, {up} connected · /mcp", servers.len()),
        );
    }
}

/// The conversation as Markdown, to `rest` or, given nothing, to a file in
/// the project folder named after the session; never over a file there.
fn export(runner: &mut Runner, session: usize, rest: &str) {
    let conversation = &runner.store.conversations[session];
    render::blank();
    if conversation.length() == 0 {
        render::line(&render::dim("Nothing to export yet."));
        return;
    }
    let folder = (conversation.workspace.clone())
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_default();
    let path = match rest.trim() {
        "" => folder.join(format!(
            "{}.md",
            file_stem(&conversation.title, conversation.id)
        )),
        named => folder.join(named),
    };
    if path.exists() {
        render::line(&render::dim(&format!(
            "{} is there already; /export <another name> writes elsewhere.",
            path.display()
        )));
        return;
    }
    let text = letronna_core::export::markdown(conversation);
    match std::fs::write(&path, text) {
        Ok(()) => render::line(&render::dim(&format!("Exported to {}.", path.display()))),
        Err(error) => render::line(&render::red(&format!(
            "Could not write {}: {error}.",
            path.display()
        ))),
    }
}

/// A file name for a session: its name in lower case, words joined by
/// hyphens, or the session's id when its name has nothing to use.
fn file_stem(title: &str, id: u64) -> String {
    let words: Vec<String> = (title.split(|c: char| !c.is_alphanumeric()))
        .filter(|word| !word.is_empty())
        .take(8)
        .map(str::to_lowercase)
        .collect();
    match words.is_empty() {
        true => format!("session-{id}"),
        false => words.join("-"),
    }
}

/// A row for each part of the context: its name, its tokens, and a bar of
/// its share of them all, a part that takes any room getting a sliver.
fn context_rows(parts: letronna_core::chat::ContextParts) -> Vec<String> {
    let rows = [
        ("system", parts.system),
        ("tools", parts.tools),
        ("MCP", parts.mcp),
        ("memory", parts.memory),
        ("skills", parts.skills),
        ("conversation", parts.conversation),
    ];
    let total = parts.total().max(1);
    (rows.into_iter())
        .map(|(name, tokens)| {
            let filled = (tokens * CONTEXT_BAR as u64).div_ceil(total) as usize;
            let filled = filled.min(CONTEXT_BAR);
            format!(
                "{}  {:>7}  {}{}",
                render::dim(&format!("{name:<12}")),
                letronna_core::usage::count(tokens),
                render::blue(&"━".repeat(filled)),
                render::dim(&"─".repeat(CONTEXT_BAR - filled)),
            )
        })
        .collect()
}

/// Columns of the bar each part of the context gets, at its fullest.
const CONTEXT_BAR: usize = 24;

/// What fills the session's context, part by part, each with a bar of its
/// share, as the window's popover behind its status-bar percentage shows
/// it: what the next request would carry, and how much of the window that
/// is.
fn context(runner: &mut Runner, session: usize) {
    let connection = runner.connection(session);
    let conversation = &runner.store.conversations[session];
    let (messages, floor) = (conversation.messages.clone(), conversation.floor());
    let report = runner.bar.busy("Adding up the context", move || {
        letronna_core::chat::context_report(&connection, &messages, floor)
    });
    let count = letronna_core::usage::count;
    render::blank();
    render::line(&render::bold(&match report.window {
        0 => format!(
            "{} in use; the model's window is not known",
            count(report.used)
        ),
        window => format!(
            "{} of {} in use · {}%",
            count(report.used),
            count(window),
            report.used.saturating_mul(100) / window
        ),
    }));
    for row in context_rows(report.parts) {
        render::line(&row);
    }
    render::line(&render::dim(
        "/compact summarizes earlier turns to make room; /compact 2 keeps the last two.",
    ));
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
            let gateway = runner.store.model(session).0;
            render::line(&render::dim(&format!(
                "{} · {}",
                crate::session::answered_by(&answer.model, gateway),
                reply_usage(&answer)
            )));
        }
        Err(error) => render::line(&render::red(&error)),
    }
}

/// Keys the box answers to. None of them are written down anywhere else but
/// the opening screen, which has scrolled away by the time anyone wonders.
pub(crate) const KEYS: &[(&str, &str)] = &[
    ("enter", "send"),
    (
        "\\+enter, ctrl+j",
        "a new line (or shift+enter or alt+enter, where they work)",
    ),
    ("tab", "finish a command or an @file name"),
    ("↑ ↓, ctrl+p/n", "step through what you said before"),
    ("ctrl+r", "search what you said before"),
    (
        "shift+tab, alt+m",
        "switch mode: asks first, edits, plan, auto",
    ),
    (
        "esc",
        "stop an answer, or clear the line (↑ brings it back)",
    ),
    ("esc esc", "go back to an earlier message"),
    (
        "alt+p, alt+t",
        "pick the model, or how hard it thinks; what is typed stays",
    ),
    (
        "ctrl+c",
        "copy, stop an answer, clear the line, or twice to leave",
    ),
    ("ctrl+d", "leave, on an empty line"),
    ("ctrl+s", "while it works: send what is queued now"),
    (
        "↑ while it works",
        "take the newest queued line back to change or drop",
    ),
    (
        "ctrl+o, click",
        "open or fold the latest thinking, or the one clicked",
    ),
    ("ctrl+b", "run the command it is running in the background"),
    ("ctrl+z", "go to the shell; fg comes back"),
    (
        "ctrl+a/e",
        "go to the start or end of the line; home and end, of the whole box",
    ),
    (
        "ctrl+u/k",
        "delete to the start or end of the line; ctrl+y puts it back",
    ),
    ("ctrl+w", "delete a word back, as alt+backspace does"),
    (
        "alt+← →",
        "move a word, as ctrl+← → and alt+b/f do; alt+d deletes one",
    ),
    ("ctrl+_", "undo the last change to the line"),
    (
        "ctrl+g",
        "write the line in your editor ($VISUAL or $EDITOR)",
    ),
    (
        "page up/down, end",
        "scroll back through the conversation; end follows it again",
    ),
    (
        "ctrl+l",
        "a clean screen, the conversation going on as it was",
    ),
    ("drag", "select and copy; a picture dragged in is attached"),
    (
        "ctrl+v",
        "attach the picture, or picture files, on the clipboard",
    ),
    ("@path", "attach a file, a line range or a folder"),
    ("//", "start a message with one /, as in //tmp is full"),
    (
        "!command",
        "run a command here; the model sees what it prints",
    ),
    ("?", "these keys, on an empty line"),
];

/// The commands, then the added ones, then the keys, each in one column so
/// it reads straight down. Skills are left to `/skills`: there can be
/// dozens, and each is a line there already.
fn help(workspace: Option<&std::path::Path>, rest: &str, moved: &[(String, String)]) {
    render::blank();
    let asked = rest.trim().trim_start_matches('/');
    if !asked.is_empty() {
        return help_for(asked);
    }
    let added: Vec<(String, String)> = listed(workspace)
        .iter()
        .filter(|c| c.origin != Origin::Skill)
        .map(|c| (format!("/{}", c.name), about(c)))
        .collect();
    let width = bar::name_width(&added);
    let row = |name: &str, blurb: &str| {
        // An added command's description is its writer's and can run long;
        // the room left is after the line's own indent, so no row wraps.
        let room = render::width().saturating_sub(render::PAD.len() + width + 2);
        render::line(&format!(
            "{}  {}",
            render::bold(&format!("{:<width$}", render::fit(name, width))),
            render::dim(&render::fit(blurb, room))
        ));
    };
    for (heading, entries) in help_groups() {
        render::line(&render::dim(heading));
        for entry in entries {
            row(entry.name, entry.blurb);
        }
        render::blank();
    }
    if !added.is_empty() {
        render::line(&render::dim("Yours, the project's and plugins'"));
        for (name, about) in &added {
            row(name, about);
        }
        render::blank();
    }
    render::line(&render::dim(
        "Skills are under /skills and MCP servers' prompts under /mcp; \
         type / and the start of a name to find one. /help <command> says more.",
    ));
    render::blank();
    keys();
    if !moved.is_empty() {
        render::blank();
        render::line(&render::dim("Moved in keybindings.json (/keybindings)"));
        for (key, what) in moved {
            row(key, what);
        }
    }
    render::blank();
    render::line(&render::dim("The guide: https://letronna.ai/docs"));
}

/// Open keybindings.json in the user's editor, made first when there is
/// none, then say where it is and what it moves now.
fn keybindings(runner: &mut Runner) {
    render::blank();
    let path = crate::keybindings::file();
    if !path.exists()
        && let Err(error) = std::fs::write(&path, crate::keybindings::template())
    {
        render::line(&render::red(&format!(
            "Could not make {}: {error}",
            path.display()
        )));
        return;
    }
    // Only with somebody at the keyboard: piped, nobody is at an editor.
    if runner.human {
        let (program, args) = crate::memory::editor();
        let program = crate::memory::found(&program);
        // Started directly, never through a shell, as /memory edit is.
        let ran = runner.bar.lend(|| {
            std::process::Command::new(&program)
                .args(&args)
                .arg(&path)
                .status()
        });
        if let Err(error) = ran {
            render::line(&render::red(&format!(
                "Could not start {}: {error}. Set VISUAL or EDITOR to the editor you use.",
                program.display()
            )));
        }
    }
    runner.bar.read_keybindings();
    render::line(&render::dim(&format!(
        "Keys are moved in {}, which is read again when it changes; it lists every action with its key.",
        path.display()
    )));
    let moved = runner.bar.moved_keys();
    if moved.is_empty() {
        render::line(&render::dim("No key is moved yet."));
    }
    for (key, what) in moved {
        render::line(&format!("{}  {}", render::bold(&key), render::dim(&what)));
    }
}

/// The keys, one to a row, as `/help` and `?` list them.
fn keys() {
    let width = KEYS
        .iter()
        .map(|(key, _)| render::visible(key))
        .max()
        .unwrap_or(0);
    let room = render::width().saturating_sub(render::PAD.len() + width + 2);
    for (key, what) in KEYS {
        render::line(&format!(
            "{}  {}",
            render::bold(&format!("{key:<width$}")),
            render::dim(&render::fit(what, room))
        ));
    }
}

/// One command, with the words it takes where the menu knows them.
fn help_for(asked: &str) {
    let name = format!("/{asked}");
    let Some(entry) = bar::COMMANDS.iter().find(|entry| entry.name == name) else {
        render::line(&render::dim(&format!(
            "{name} is not one of Letronna's commands; type / to see them."
        )));
        return;
    };
    render::said(&render::blue("●"), &render::bold(entry.name));
    render::line(&render::dim(entry.blurb));
    let words = bar::words_for(entry.name);
    if !words.is_empty() {
        render::blank();
        let width = words
            .iter()
            .map(|(word, _)| render::visible(word))
            .max()
            .unwrap_or(0);
        let room = render::width().saturating_sub(render::PAD.len() + width + 2);
        for (word, about) in &words {
            render::line(&format!(
                "{}  {}",
                render::bold(&format!("{word:<width$}")),
                render::dim(&render::fit(about, room))
            ));
        }
    }
}

/// The headings `/help` puts Letronna's own commands under, in the order it
/// shows them. A name listed that is not a command here is passed over, and
/// a command under no heading goes under the last.
const HELP_GROUPS: &[(&str, &[&str])] = &[
    (
        "Sessions",
        &[
            "/new",
            "/resume",
            "/rename",
            "/branch",
            "/background",
            "/rewind",
            "/undo",
            "/redo",
            "/retry",
            "/compact",
            "/context",
            "/btw",
            "/recap",
            "/copy",
            "/export",
            "/status",
            "/statusline",
            "/diff",
            "/where",
            "/window",
            "/import",
            "/retention",
            "/clear",
        ],
    ),
    (
        "Model",
        &[
            "/model",
            "/local",
            "/effort",
            "/thinking",
            "/style",
            "/fallback",
            "/voice",
        ],
    ),
    (
        "Modes",
        &["/mode", "/plan", "/decisions", "/permissions", "/verify", "/format", "/coauthor"],
    ),
    (
        "Tools",
        &[
            "/tools",
            "/search",
            "/tool-search",
            "/skills",
            "/mcp",
            "/plugins",
            "/image",
            "/preview",
        ],
    ),
    (
        "Memory",
        &["/memory", "/remember", "/memory-review", "/init"],
    ),
    (
        "Background work",
        &[
            "/goal",
            "/loop",
            "/heartbeat",
            "/jobs",
            "/agents",
            "/workflows",
            "/crew",
            "/notify",
        ],
    ),
    (
        "Account",
        &[
            "/connect",
            "/team",
            "/login",
            "/logout",
            "/whoami",
            "/usage",
            "/limits",
            "/insights",
            "/wallet",
        ],
    ),
    ("Help", &["/help", "/keybindings", "/vim", "/theme", "/terminal-setup", "/exit"]),
];

/// Letronna's own commands under their headings, those under none last.
fn help_groups() -> Vec<(&'static str, Vec<bar::Command<'static>>)> {
    let find = |name: &&str| {
        bar::COMMANDS
            .iter()
            .find(|entry| entry.name == *name)
            .copied()
    };
    let mut groups: Vec<(&'static str, Vec<bar::Command<'static>>)> = (HELP_GROUPS.iter())
        .map(|(heading, names)| (*heading, names.iter().filter_map(find).collect()))
        .collect();
    let grouped: Vec<&str> = HELP_GROUPS
        .iter()
        .flat_map(|(_, names)| names.iter().copied())
        .collect();
    let rest: Vec<bar::Command<'static>> = (bar::COMMANDS.iter())
        .filter(|entry| !grouped.contains(&entry.name))
        .copied()
        .collect();
    if let Some((_, last)) = groups.last_mut() {
        last.extend(rest);
    }
    groups.retain(|(_, entries)| !entries.is_empty());
    groups
}

/// The switch `rest` asks `command` for: on or off, or the other way from
/// `now` when it names neither. `None`, once it has said how, for any
/// other word, which used to flip the switch whatever it said.
fn switched(command: &str, rest: &str, now: bool) -> Option<bool> {
    match rest.trim() {
        "" => Some(!now),
        "on" => Some(true),
        "off" => Some(false),
        _ => {
            render::line(&render::dim(&format!(
                "{command} on or {command} off; {command} alone switches it."
            )));
            None
        }
    }
}

/// Name Letronna as co-author of what a session here commits, and put its
/// mark on a pull request it writes, or not; with `cloud`, the same for
/// Letronna Cloud's sessions, which the account keeps.
fn coauthor(runner: &mut Runner, rest: &str) {
    let rest = rest.trim();
    if let Some(rest) = rest.strip_prefix("cloud") {
        let Some(account) = letronna_core::auth::account() else {
            render::blank();
            render::line(&render::red(
                "Letronna Cloud's choice is your account's: /login first.",
            ));
            return;
        };
        let now = account.letronna_coauthor.unwrap_or(true);
        let Some(on) = switched("/coauthor cloud", rest, now) else {
            return;
        };
        render::blank();
        match letronna_core::auth::set_letronna_coauthor(on) {
            Ok(_) if on => render::line(&render::dim(
                "Letronna Cloud's sessions name Letronna as co-author, and mark their pull requests.",
            )),
            Ok(_) => render::line(&render::dim(
                "Letronna Cloud's sessions' commits and pull requests are yours alone.",
            )),
            Err(why) => render::line(&render::red(&why)),
        }
        return;
    }
    let Some(on) = switched("/coauthor", rest, runner.store.settings.letronna_coauthor) else {
        return;
    };
    runner.store.settings.letronna_coauthor = on;
    runner.persist();
    render::blank();
    render::line(&render::dim(if on {
        "A commit a session writes here ends with \u{201c}Co-authored-by: Letronna\u{201d}, and a pull request with Letronna's mark."
    } else {
        "Commits and pull requests written here are yours alone."
}

/// Turn running the project's own formatter on each file the agent writes
/// on or off for the project `here`, saying what it would use.
fn format_edits(here: Option<&std::path::Path>, rest: &str) {
    render::blank();
    let Some(root) = here else {
        render::line(&render::dim(
            "This session has no project folder to format in.",
        ));
        return;
    };
    let Some(on) = switched("/format", rest, letronna_core::formatter::on(root)) else {
        return;
    };
    if let Err(error) = letronna_core::formatter::switch(root, on) {
        render::line(&render::red(&error));
        return;
    }
    if !on {
        render::line(&render::dim(
            "Files the model writes here are left as it writes them.",
        ));
        return;
    }
    let found: Vec<String> = letronna_core::formatter::found(root)
        .iter()
        .map(|(formatter, kinds)| format!("{} for {kinds}", formatter.name))
        .collect();
    render::line(&render::dim(&if found.is_empty() {
        "On, but nothing is set up at the top of this project: rustfmt in a Cargo project, \
         prettier or Biome in node_modules under their config, ruff or black in pyproject.toml, \
         gofmt beside go.mod, clang-format under .clang-format. One set up deeper in is used \
         for the files under it."
            .to_owned()
    } else {
        format!(
            "Each file the model writes here now goes through {}.",
            found.join(", ")
        )
    }));
}

/// Turn reading replies aloud on or off.
fn voice(runner: &mut Runner, on: bool) {
    render::blank();
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
    // Over SSH the browser cannot reach this machine's port, so the page
    // offers a code instead.
    render::line(&render::dim(
        "On another machine? Press esc, then type the code the page shows.",
    ));
    open_in_browser(&pending.url);

    let stop = pending.cancel();
    let state = pending.state();
    let signed_in = match runner.bar.busy_cancellable(
        "waiting for the browser · esc for a code or to cancel",
        stop,
        move || pending.finish(),
    ) {
        Err(error) if error == "Sign-in cancelled." => {
            match runner
                .bar
                .ask("The code from the sign-in page, or esc to cancel", true)
            {
                Some(code) if !code.trim().is_empty() => {
                    runner.bar.busy("checking the code", move || {
                        letronna_core::auth::redeem(&state, &code)
                    })
                }
                _ => Err(error),
            }
        }
        done => done,
    };
    match signed_in {
        Ok(account) => {
            render::said(&render::blue("›"), &render::bold(&account.name));
            let plan = account.plan_name();
            render::line(&render::dim(&if plan.is_empty() {
                "Signed in.".to_owned()
            } else {
                format!("Signed in · {plan} · {} credit", account.credit())
            }));
            // Signing in changes no model, so say where Letronna's are.
            let settings = &runner.store.settings;
            if account.needs_upgrade() {
                // Those a perk makes free today are open all the same.
                let free = (account.perks.iter()).any(|p| p.available && p.share >= 100);
                let but = match free {
                    true => ", but for those free today",
                    false => "",
                };
                render::line(&render::dim(&format!(
                    "Its hosted models need a subscription on the Free plan{but}: {}",
                    letronna_core::auth::checkout()
                )));
            } else if settings.gateway != letronna_core::gateway::LETRONNA
                || settings.model.is_empty()
            {
                render::line(&render::dim(
                    "Letronna's models are in /model now, beside any others you set up.",
                ));
            }
        }
        // Given up on with escape: what was asked for, not a failure.
        Err(error) if error == "Sign-in cancelled." => render::line(&render::dim(&error)),
        Err(error) => render::line(&render::red(&error)),
    }
}

/// Sign this computer out of Letronna: /logout, and /connect's choice.
fn sign_out(runner: &mut Runner) {
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
            // A model on Letronna needs the sign-in, so its next message
            // would only be refused.
            if runner.store.settings.gateway == letronna_core::gateway::LETRONNA {
                render::line(&render::dim(
                    "New messages were going to Letronna's models: /login again, or pick another with /model.",
                ));
            }
        }
        (None, Ok(Some(note))) => {
            render::line(&render::dim(&format!("Signed out here. {note}")));
        }
        (None, Ok(None)) => render::line(&render::dim("Nobody was signed in.")),
    }
}

/// Sign in to a provider in the browser, or by pasting the address the
/// browser ended on when it cannot reach this computer.
fn provider_sign_in(runner: &mut Runner, name: &'static str) {
    use letronna_core::providers::auth;
    use std::sync::atomic::{AtomicBool, Ordering};
    render::blank();
    if let Some(p) = letronna_core::providers::find(name)
        && let Some(caveat) = p.caveat()
    {
        render::line(&render::dim(p.info().blurb));
        render::line(&render::dim(caveat));
    }
    let flow = match auth::begin(name) {
        Ok(flow) => flow,
        Err(error) => return render::line(&render::red(&error)),
    };
    render::said(
        &render::blue("●"),
        &render::bold(&format!("Sign in to {name}")),
    );
    render::line(&render::dim(if flow.open_browser() {
        "Opening your browser. Come back when it is done."
    } else {
        "Open this in your browser:"
    }));
    render::line(&render::dim(&flow.url));
    let waiting = {
        let flow = flow.clone();
        std::thread::spawn(move || flow.finish())
    };
    let pasted = runner
        .bar
        .ask(
            "Press enter once signed in, or paste the address the browser ended on",
            false,
        )
        .unwrap_or_default();
    if !pasted.trim().is_empty()
        && let Err(error) = flow.paste(pasted.trim())
    {
        flow.cancel();
        return render::line(&render::red(&error));
    }
    let stop = std::sync::Arc::new(AtomicBool::new(false));
    let watch = (stop.clone(), flow.clone());
    let done = runner.bar.busy_cancellable(
        "waiting for the browser · esc to cancel",
        stop,
        move || {
            while !waiting.is_finished() {
                if watch.0.load(Ordering::Relaxed) {
                    watch.1.cancel();
                }
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
            waiting
                .join()
                .unwrap_or_else(|_| Err("The sign-in stopped.".into()))
        },
    );
    match done {
        Ok(tokens) => {
            let usage = letronna_core::providers::router::usage(name);
            let usage = if usage.is_empty() {
                tokens.email
            } else {
                usage
            };
            render::line(&render::dim(&format!("Signed in to {name}: {usage}.")));
            check_gateway(runner, name);
        }
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
                Some(index) => slid_to(stops[index], current, &saved),
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
    // A word that is no level is said so, rather than taken for auto.
    if sent.is_none() && chosen != "auto" {
        render::blank();
        render::line(&render::dim(&format!(
            "{chosen} is not a level {model} has: {}.",
            stops.join(", ")
        )));
        return;
    }
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

/// What picking `picked` on the effort slider asks for, where it stood at
/// `current`: where it already was, a level saved on a model that goes
/// further stays saved for when that model is back, and auto, saved as
/// nothing, stays auto.
fn slid_to(picked: &str, current: &str, saved: &str) -> String {
    match picked == current && current != "auto" {
        true => saved.to_owned(),
        false => picked.to_owned(),
    }
}

/// The terminal's colours: bare on a keyboard, each theme drawn as it looks
/// and one picked from them; bare off one, the themes with the one in use
/// marked; or `name` chosen.
fn theme(runner: &mut Runner, name: &str) {
    use crate::theme::{self as themes, AUTO, THEMES};
    render::blank();
    let saved = runner.store.settings.terminal_theme.clone();
    let here = match themes::light_background() {
        Some(true) => "light here",
        Some(false) => "dark here",
        None => "dark here, as the terminal has not said",
    };
    let auto_about = format!("light or dark as the terminal's background is ({here})");
    let rows: Vec<(String, String, bool)> =
        std::iter::once((AUTO.to_owned(), auto_about, themes::is_auto(&saved)))
            .chain(THEMES.iter().map(|theme| {
                let using = themes::setting_for(&saved).as_deref()
                    == themes::setting_for(theme.name).as_deref();
                (theme.name.to_owned(), theme.about.to_owned(), using)
            }))
            .collect();
    let width = rows
        .iter()
        .map(|(name, ..)| name.chars().count())
        .max()
        .unwrap_or(0);
    let picked = if !name.is_empty() {
        name.to_owned()
    } else if runner.bar.steered() {
        // Each drawn in its own colours, whatever is drawn with now.
        if render::colour() {
            for theme in THEMES {
                for (at, row) in themes::sample(theme).iter().enumerate() {
                    let name = if at == 0 { theme.name } else { "" };
                    let name = render::bold(&format!("{name:<width$}"));
                    render::line(&format!("{name}  {row}"));
                }
            }
            render::blank();
        }
        let labels: Vec<(String, String)> = (rows.iter())
            .map(|(name, about, using)| {
                let using = if *using { "  (in use)" } else { "" };
                (name.clone(), format!("{name}  ·  {about}{using}"))
            })
            .collect();
        let Some(picked) = pick_name(runner, "Colours", &labels) else {
            return;
        };
        picked
    } else {
        for (name, about, using) in &rows {
            let mark = if *using {
                render::blue("●")
            } else {
                " ".into()
            };
            render::line(&format!(
                "{mark} {}  {}",
                render::bold(&format!("{name:<width$}")),
                render::dim(about)
            ));
        }
        render::line(&render::dim("Pick one with /theme and its name."));
        return;
    };
    let Some(setting) = themes::setting_for(&picked) else {
        render::line(&render::red(&format!(
            "No theme called {picked}. /theme lists them."
        )));
        return;
    };
    runner.store.settings.terminal_theme = setting.clone();
    runner.persist();
    themes::choose(&setting);
    let note = footer(runner);
    runner.bar.set_footer(note);
    render::line(&render::dim(&match setting.as_str() {
        "" => "Colours: dark, as Letronna has always looked.".to_owned(),
        AUTO => match themes::light_background() {
            Some(light) => format!(
                "Colours follow the terminal's background: {} here.",
                if light { "light" } else { "dark" }
            ),
            None if themes::asked_at_start() => "Colours follow the terminal's background; \
                 this one did not say, so dark."
                .to_owned(),
            None => "Colours follow the terminal's background, which it is asked at the \
                     next start; dark until then."
                .to_owned(),
        },
        name => format!("Colours: {name}."),
    }));
}

/// The status line: what it runs and shows, the input it is given, or a
/// command set for it or taken away with off. The main loop runs it again
/// after every command, so a new one shows at once.
fn status_line(runner: &mut Runner, session: usize, rest: &str) {
    render::blank();
    match rest {
        "" => {
            let command = runner.store.settings.status_line.trim().to_owned();
            if command.is_empty() {
                render::line(&render::dim(
                    "No status line. /statusline <command> shows the first line it prints \
                     under the footer. It is given the session as JSON on its input, as Claude \
                     Code's statusLine is, so a script for that works here.",
                ));
                return;
            }
            render::line(&format!("{}  {command}", render::dim("runs ")));
            match runner.bar.status() {
                Some(shown) => {
                    // Its own colours, ended after it, or none where colour is off.
                    let shown = match render::colour() {
                        true => format!("{shown}\x1b[0m"),
                        false => render::plain(&shown),
                    };
                    render::line(&format!("{}  {shown}", render::dim("shows")));
                }
                None => render::line(&render::dim("It has printed nothing yet.")),
            }
            render::line(&render::dim(
                "/statusline input shows what it is given; /statusline off takes it away.",
            ));
        }
        "input" => {
            let input = runner.status_input(session);
            let text = serde_json::to_string_pretty(&input).unwrap_or_default();
            for line in text.lines() {
                render::line(&render::dim(line));
            }
        }
        "off" | "clear" | "none" => {
            runner.store.settings.status_line.clear();
            runner.persist();
            render::line(&render::dim("Status line off."));
        }
        command => {
            runner.store.settings.status_line = command.to_owned();
            runner.persist();
            render::line(&render::dim(
                "Status line on: its first line shows under the footer, run again after \
                 each command and turn, at most every 0.3 s.",
            ));
        }
    }
}

/// The reply styles with the chosen one marked, or `name` chosen.
fn style(runner: &mut Runner, name: &str) {
    use letronna_core::styles;
    render::blank();
    // Bare, on a keyboard, the styles are offered with the one in use marked.
    if name.is_empty() && runner.bar.steered() {
        let current = styles::current().map(|s| s.name);
        let rows: Vec<(String, String)> = std::iter::once((
            styles::DEFAULT.to_owned(),
            "no style, as Letronna usually writes".to_owned(),
        ))
        .chain(styles::all().into_iter().map(|s| (s.name, s.about)))
        .map(|(name, about)| {
            let using = match current.as_deref() {
                Some(current) if current == name => "  (in use)",
                None if name == styles::DEFAULT => "  (in use)",
                _ => "",
            };
            let label = format!("{name}  ·  {about}{using}");
            (name, label)
        })
        .collect();
        if let Some(picked) = pick_name(runner, "Reply style", &rows) {
            style(runner, &picked);
        }
        return;
    }
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
fn jobs(runner: &mut Runner, rest: &str) {
    let running = letronna_core::jobs::list();
    render::blank();
    if running.is_empty() {
        render::said(&render::blue("●"), &render::bold("Nothing running"));
        return;
    }

    // `/jobs stop 3`, or a bare `/jobs` to look.
    if let Some(id) = rest.strip_prefix("stop").map(str::trim) {
        let rows: Vec<(String, String)> = running
            .iter()
            .filter(|job| job.running)
            .map(|job| {
                let command = letronna_core::tools::one_line(&job.command);
                (job.id.to_string(), format!("{}  {command}", job.id))
            })
            .collect();
        let id = match id.is_empty() && runner.bar.steered() && !rows.is_empty() {
            true => match pick_name(runner, "Stop which job?", &rows) {
                Some(id) => id,
                None => return,
            },
            false => id.to_owned(),
        };
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

    let heading = match (
        running.iter().any(|j| j.running),
        running.iter().any(|j| !j.running),
    ) {
        (true, false) => "Running",
        (false, true) => "Finished",
        _ => "Jobs",
    };
    render::said(&render::blue("●"), &render::bold(heading));
    for job in &running {
        let state = if job.running { "running" } else { "finished" };
        render::line(&format!(
            "{}  {}  {}",
            render::bold(&job.id.to_string()),
            letronna_core::tools::one_line(&job.command),
            render::dim(state)
        ));
    }
    render::line(&render::dim("/jobs stop to pick one to stop"));
}

/// Words that go with `/workflows`, shown after a list and for words it
/// could not read.
const RUN_USAGE: &str = "/workflows <run>  ·  /workflows stop|pause|resume <run>  ·  /workflows \
                         skip|retry <run> <agent>";

/// What `/workflows` was asked, read from the words after it.
#[derive(Debug, PartialEq)]
enum Asked<'a> {
    List,
    Show(String),
    Stop(String),
    Resume(String),
    Pause(String),
    /// A run and one of its agents, by label or `#N`.
    Skip(String, String),
    Retry(String, String),
    /// A verb that works on a run, given none.
    Which(&'a str),
    /// A verb that works on an agent, given a run but no agent.
    WhichAgent(&'a str, String),
    Unclear,
}

/// Reads the words after `/workflows`: nothing to list the runs, a run to
/// show it, a verb and a run, or for skip and retry a run and one of its
/// agents, whose label may take several words. A run is `wf-3`, or just `3`.
fn asked(rest: &str) -> Asked<'_> {
    let words: Vec<&str> = rest.split_whitespace().collect();
    match words[..] {
        [] => Asked::List,
        [verb @ ("show" | "stop" | "resume" | "pause" | "skip" | "retry")] => Asked::Which(verb),
        [verb @ ("skip" | "retry"), run] => Asked::WhichAgent(verb, run_name(run)),
        ["skip", run, ref agent @ ..] => Asked::Skip(run_name(run), agent.join(" ")),
        ["retry", run, ref agent @ ..] => Asked::Retry(run_name(run), agent.join(" ")),
        ["show", run] | [run] => Asked::Show(run_name(run)),
        ["stop", run] => Asked::Stop(run_name(run)),
        ["pause", run] => Asked::Pause(run_name(run)),
        ["resume", run] => Asked::Resume(run_name(run)),
        _ => Asked::Unclear,
    }
}

/// A run by the name the list gives it, so `3` means `wf-3`.
fn run_name(word: &str) -> String {
    if word.bytes().all(|byte| byte.is_ascii_digit()) {
        format!("wf-{word}")
    } else {
        word.to_owned()
    }
}

/// One of this session's workflow runs, as `/workflows` shows it.
struct Run {
    name: String,
    /// Its meta's name.
    title: String,
    /// How it stands, with how far it has got while it runs.
    state: String,
    /// What it has spent, as a notice puts it.
    tokens: String,
    stage: Stage,
}

/// Where a run is, as far as what can be done with it goes.
enum Stage {
    Running,
    /// Held by a pause, until a resume lets it go on where it is.
    Paused,
    Finished,
    /// Stopped, failed or paused, so a resume picks it up.
    Ended,
    /// Still going when Letronna closed, so nothing drives it now.
    Closed(LeftOpen),
}

/// Session `id`'s workflow runs, and what can be done with them.
///
/// A run goes on in the background and says nothing until it ends, so this
/// is where the user looks in on one, pauses or stops it, skips or retries
/// one of its agents, or picks it back up. Only the session that started a
/// run is shown it, as only its own agent may stop it.
fn workflows(runner: &mut Runner, session: usize, rest: &str) {
    let id = runner.store.conversations[session].id;
    render::blank();
    match asked(rest) {
        Asked::List => list_runs(id),
        Asked::Show(name) => show_run(id, &name),
        Asked::Stop(name) => stop_run(id, &name),
        Asked::Resume(name) => {
            // Its notice is taken as a turn at the prompt, which a stopped
            // turn would hold back until the user says something: they have.
            if resume_run(id, &name) {
                runner.stopped = false;
            }
        }
        Asked::Pause(name) => pause_run(id, &name),
        Asked::Skip(name, agent) => skip_agent(id, &name, &agent),
        Asked::Retry(name, agent) => retry_agent(id, &name, &agent),
        Asked::Which(verb) => render::said(
            &render::blue("●"),
            &render::bold(&format!("Which one? /workflows {verb} <run>")),
        ),
        Asked::WhichAgent(verb, name) => {
            render::said(
                &render::blue("●"),
                &render::bold(&format!("Which agent? /workflows {verb} {name} <agent>")),
            );
            render::line(&render::dim(&format!(
                "An agent is its label or #N, as /workflows {name} lists them."
            )));
        }
        Asked::Unclear => render::line(&render::dim(&format!(
            "/workflows to list the runs  ·  {RUN_USAGE}"
        ))),
    }
}

/// Session `id`'s workflow runs, oldest first, then those Letronna closed on
/// while they ran.
fn runs(id: u64) -> Vec<Run> {
    let mut runs: Vec<Run> = (workflow::runs(id).iter())
        .map(|view| Run {
            name: view.name.clone(),
            title: view.title.clone(),
            state: standing(view),
            tokens: short(view.spent),
            stage: match view.state {
                RunState::Running => Stage::Running,
                RunState::Paused => Stage::Paused,
                RunState::Finished => Stage::Finished,
                RunState::Ended(_) => Stage::Ended,
            },
        })
        .collect();
    let closed = (left_open().into_iter()).filter(|run| run.session == id);
    runs.extend(closed.map(|run| Run {
        name: run.name.clone(),
        title: run.title.clone(),
        state: format!("closed at {} of {} done", run.done, run.seen),
        tokens: "-".into(),
        stage: Stage::Closed(run),
    }));
    runs
}

/// Asks the `agents` tool something as session `id`'s own agent, which is
/// who may stop and report its workflow runs.
fn agents_call(id: u64, args: Value) -> Result<String, String> {
    let connection = Connection {
        conversation: id,
        ..Connection::default()
    };
    letronna_core::agents::Agents.run(&args, &ToolContext::of(&connection))
}

/// How a run stands and for how long, with how far it has got while it
/// goes: "running · 1m02s · 3 of 9 done", "stopped · 9s".
pub(crate) fn standing(view: &RunView) -> String {
    let agents = view.phases.iter().flat_map(|phase| &phase.agents);
    let (calls, done) = agents.fold((0, 0), |(calls, done), agent| {
        (calls + 1, done + usize::from(agent.state.answered()))
    });
    let took = took(view.seconds);
    match &view.state {
        RunState::Running => format!("running · {took} · {done} of {calls} done"),
        RunState::Paused => format!("paused · {took} · {done} of {calls} done"),
        RunState::Finished => format!("finished · {took}"),
        RunState::Ended(how) => format!("{how} · {took}"),
    }
}

/// A run's agents under their phases, one line apiece with its number,
/// label, state and spend, the labels lined up.
fn agent_lines(view: &RunView) -> Vec<String> {
    let agents = || view.phases.iter().flat_map(|phase| &phase.agents);
    let wide = (agents().map(|agent| agent.label.chars().count()).max()).unwrap_or(0);
    let mut lines = Vec::new();
    for phase in &view.phases {
        if !phase.title.is_empty() {
            lines.push(phase.title.clone());
        }
        for agent in &phase.agents {
            let tokens = match agent.tokens {
                0 => "-".to_owned(),
                tokens => short(tokens),
            };
            let number = format!("#{}", agent.number);
            let line = format!(
                "  {number:<4} {:<wide$}  {:<9} {tokens}",
                agent.label,
                agent.state.word()
            );
            lines.push(line);
        }
    }
    lines
}

/// The runs as a table, a header first, each row cut to `width`. The title
/// comes last, so a narrow window loses that before what tells runs apart.
fn run_rows(runs: &[Run], width: usize) -> Vec<String> {
    let mut rows = vec![["run", "state", "tokens", "workflow"].map(String::from)];
    rows.extend(runs.iter().map(|run| {
        let cells = [&run.name, &run.state, &run.tokens, &run.title];
        cells.map(String::clone)
    }));
    // Every column before the title as wide as its widest cell, so they line up.
    let columns: Vec<usize> = (0..3)
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
            line.push_str(&row[3]);
            render::clip(line.trim_end(), width)
        })
        .collect()
}

fn list_runs(id: u64) {
    let runs = runs(id);
    if runs.is_empty() {
        render::said(
            &render::blue("●"),
            &render::bold("No workflow runs in this session"),
        );
        render::line(&render::dim(
            "/crew on, @crew in a message or a saved workflow's command lets Letronna start one.",
        ));
        return;
    }
    render::said(&render::blue("●"), &render::bold("Workflow runs"));
    let width = render::width().saturating_sub(render::PAD.len());
    let rows = run_rows(&runs, width);
    let Some((header, rows)) = rows.split_first() else {
        return;
    };
    render::line(&render::dim(header));
    for (row, run) in rows.iter().zip(&runs) {
        // One that is not going stands back from those at work.
        render::line(&match run.stage {
            Stage::Running | Stage::Paused => row.clone(),
            _ => render::dim(row),
        });
    }
    render::line(&render::dim(RUN_USAGE));
}

/// The run called `name`, or None once the user has been told there is none.
fn named_run<'a>(runs: &'a [Run], name: &str) -> Option<&'a Run> {
    let found = runs.iter().find(|run| run.name == name);
    if found.is_none() {
        let names: Vec<&str> = runs.iter().map(|run| run.name.as_str()).collect();
        let mut text = format!("No workflow run called \"{name}\" in this session.");
        if !names.is_empty() {
            text.push_str(&format!(" Its runs: {}.", names.join(", ")));
        }
        render::line(&render::dim(&text));
    }
    found
}

/// One run: how it stands, its agents under their phases, then its result
/// once it has one.
fn show_run(id: u64, name: &str) {
    let runs = runs(id);
    let Some(run) = named_run(&runs, name) else {
        return;
    };
    render::said(
        &render::blue("●"),
        &render::bold(&format!("{name} \"{}\"", run.title)),
    );
    let mut state = run.state.clone();
    if run.tokens != "-" {
        state.push_str(&format!("  ·  {} tokens", run.tokens));
    }
    render::line(&render::dim(&state));
    if let Stage::Closed(_) = run.stage {
        render::line(&render::dim(&format!(
            "Letronna closed while it ran. /workflows resume {name} picks it up."
        )));
        return;
    }
    if let Ok(view) = workflow::run(id, name) {
        let width = render::width().saturating_sub(render::PAD.len());
        for line in agent_lines(&view) {
            render::line(&render::dim(&render::clip(&line, width)));
        }
    }
    match agents_call(id, json!({"action": "report", "to": name})) {
        Ok(report) => report.lines().for_each(render::line),
        Err(error) => render::line(&render::red(&error)),
    }
}

/// Stop run `name`, keeping what its finished agents said for a resume.
fn stop_run(id: u64, name: &str) {
    let runs = runs(id);
    let Some(run) = named_run(&runs, name) else {
        return;
    };
    let not_going = match run.stage {
        Stage::Running | Stage::Paused => None,
        Stage::Closed(_) => Some(format!(
            "{name} is not running: Letronna closed while it ran."
        )),
        Stage::Finished | Stage::Ended => {
            Some(format!("{name} has already ended ({}).", run.state))
        }
    };
    if let Some(text) = not_going {
        render::line(&render::dim(&text));
        return;
    }
    match agents_call(id, json!({"action": "stop", "to": name})) {
        // It ends at its next check, not this instant, so the list may
        // still show it running for a moment.
        Ok(_) => {
            render::said(
                &render::blue("●"),
                &render::bold(&format!("Stopping {name}")),
            );
            render::line(&render::dim(&format!(
                "Its finished agents' answers are kept: /workflows resume {name} picks it up."
            )));
        }
        Err(error) => render::line(&render::red(&error)),
    }
}

/// Picks run `name` back up, and says whether the model was asked to: a
/// paused run goes on where it is, and one that ended shows its launch card
/// again, its finished agents' answers coming back at no cost.
fn resume_run(id: u64, name: &str) -> bool {
    let runs = runs(id);
    let Some(run) = named_run(&runs, name) else {
        return false;
    };
    match &run.stage {
        Stage::Running => {
            render::line(&render::dim(&format!("{name} is still running.")));
            return false;
        }
        Stage::Paused => {
            match workflow::resume(id, name) {
                Ok(()) => render::said(
                    &render::blue("●"),
                    &render::bold(&format!("Resuming {name}")),
                ),
                Err(error) => render::line(&render::dim(&error)),
            }
            return false;
        }
        Stage::Finished => {
            render::line(&render::dim(&format!(
                "{name} has finished; /workflows {name} shows its result."
            )));
            return false;
        }
        Stage::Closed(left) => letronna_core::crew::resume(left),
        // As `crew::resume` asks for one Letronna closed on, for a run this
        // session stopped, or that failed or paused.
        Stage::Ended => {
            letronna_core::crew::grant(id);
            let call = json!({"resume": name});
            let text = format!(
                "The user chose to resume workflow {name} \"{}\", which ended ({}): call \
                 workflow {call}.",
                run.title, run.state
            );
            let notice = Notice {
                from: "Crew".into(),
                when: When::Next,
                text,
                files: Vec::new(),
            };
            inbox::push(id, notice);
        }
    }
    render::said(
        &render::blue("●"),
        &render::bold(&format!("Resuming {name}")),
    );
    true
}

/// Holds run `name` where it is: its running agents finish, and no other
/// starts until a resume.
fn pause_run(id: u64, name: &str) {
    if let Err(error) = workflow::pause(id, name) {
        render::line(&render::dim(&error));
        return;
    }
    render::said(
        &render::blue("●"),
        &render::bold(&format!("Pausing {name}")),
    );
    render::line(&render::dim(&format!(
        "Its running agents finish; no other starts until /workflows resume {name}."
    )));
}

/// Gives run `name`'s script null for `agent`, stopping it if it runs.
fn skip_agent(id: u64, name: &str, agent: &str) {
    if let Err(error) = workflow::skip(id, name, agent) {
        render::line(&render::dim(&error));
        return;
    }
    render::said(
        &render::blue("●"),
        &render::bold(&format!("Skipping {agent} of {name}")),
    );
    render::line(&render::dim(
        "Its script goes on without its answer, and a resume skips it again.",
    ));
}

/// Sets `agent` of run `name` to run again when the run is next resumed.
fn retry_agent(id: u64, name: &str, agent: &str) {
    if let Err(error) = workflow::retry(id, name, agent) {
        render::line(&render::dim(&error));
        return;
    }
    render::said(
        &render::blue("●"),
        &render::bold(&format!("{agent} of {name} runs again at its next resume")),
    );
    // The script has gone on without its answer, so only a resume, which
    // runs the script again, can use a new one.
    let going = workflow::run(id, name)
        .is_ok_and(|view| matches!(view.state, RunState::Running | RunState::Paused));
    render::line(&render::dim(&if going {
        format!(
            "{name} is still going: /workflows stop {name}, then /workflows resume {name}, \
             runs it now."
        )
    } else {
        format!("/workflows resume {name} runs it, and whatever used its answer.")
    }));
}

/// Whether `/crew` with `rest` after it turns Crew on or off in a session
/// where it is `on` now, or None for words it does not take.
fn crew_switch(rest: &str, on: bool) -> Option<bool> {
    match rest {
        "" => Some(!on),
        "on" => Some(true),
        "off" => Some(false),
        _ => None,
    }
}

/// Crew on or off in session `id`: while it is on, Letronna may plan a
/// workflow of helpers for a big task, each run still asking first.
fn crew(id: u64, rest: &str) {
    render::blank();
    let Some(on) = crew_switch(rest, letronna_core::crew::on(id)) else {
        render::line(&render::dim(
            "/crew to turn it over  ·  /crew on  ·  /crew off",
        ));
        return;
    };
    letronna_core::crew::set(id, on);
    render::line(&render::dim(if on {
        "Crew on: Letronna may run a workflow of helpers for a big task. Each run asks first."
    } else {
        "Crew off: workflows run only when you ask, with @crew or a saved workflow."
    }));
}

/// This session's helpers and how each is getting on, or one of them stopped
/// or told something.
///
/// A helper in the background says nothing until it ends, and the model is
/// told not to ask after it, so this is where the user looks in on one.
fn agents(runner: &mut Runner, session: u64, rest: &str) {
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
    // With no name, on a keyboard, the helpers still at work are offered.
    let working: Vec<(String, String)> = helpers
        .iter()
        .filter(|helper| helper.ended.is_none())
        .map(|helper| {
            let task = letronna_core::tools::one_line(&helper.description);
            (
                helper.name.clone(),
                format!("{}  ·  {}  ·  {task}", helper.name, helper.kind),
            )
        })
        .collect();
    let rest = rest.trim();
    let picking = rest.is_empty() && runner.bar.steered() && !working.is_empty();
    match action {
        "stop" if picking => {
            if let Some(name) = pick_name(runner, "Stop which helper?", &working) {
                stop_helper(&helpers, &name);
            }
        }
        "send" if picking => {
            let Some(name) = pick_name(runner, "Tell which helper?", &working) else {
                return;
            };
            match runner.bar.ask(&format!("What should {name} hear?"), false) {
                Some(text) if !text.trim().is_empty() => {
                    send_to_helper(&helpers, &format!("{name} {}", text.trim()));
                }
                _ => render::line(&render::dim("Nothing sent.")),
            }
        }
        "stop" => stop_helper(&helpers, rest),
        "send" => send_to_helper(&helpers, rest),
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
            _ if helper.idle => "idle, waiting for more work".to_owned(),
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
    // The stop looks again, since the helper may have ended since the list
    // was read. A kept one waiting for more work is stopped too.
    if (helper.ended.is_none() || helper.idle) && letronna_core::agents::stop(helper.id) {
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

/// The words `/mcp` takes before a server's name.
const MCP_VERBS: [&str; 7] = [
    "reconnect",
    "enable",
    "disable",
    "login",
    "logout",
    "log",
    "reload",
];

/// The MCP servers and how each is doing, or reconnect, enable, disable,
/// sign in to or out of or read the log of one, or read mcp.json again.
fn mcp_servers(runner: &mut Runner, rest: &str) {
    use letronna_core::mcp;
    let (verb, name) = rest.split_once(' ').unwrap_or((rest, ""));
    let name = name.trim();
    render::blank();
    if matches!(verb, "enable" | "disable") && name.is_empty() && switch_servers(runner) {
        return;
    }
    // The other verbs that need a server, with none named, offer them.
    let wants_one = matches!(verb, "reconnect" | "login" | "logout" | "log");
    let servers: Vec<(String, String)> = match wants_one && name.is_empty() {
        true => mcp::status()
            .iter()
            .map(|server| {
                let health = render::plain(&crate::mcp::health(server));
                (server.name.clone(), format!("{}  ·  {health}", server.name))
            })
            .collect(),
        false => Vec::new(),
    };
    let picked = match runner.bar.steered() && !servers.is_empty() {
        true => match pick_name(runner, &server_question(verb), &servers) {
            Some(server) => server,
            None => return,
        },
        false => name.to_owned(),
    };
    let name = picked.as_str();
    // A word that is none of them is said so, server names included, rather
    // than asked which server it is for.
    if !verb.is_empty() && !MCP_VERBS.contains(&verb) {
        render::line(&render::red(&format!(
            "/mcp {verb} is not one of {}; /mcp alone lists the servers.",
            MCP_VERBS.join(", ")
        )));
        return;
    }
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
    // What the servers offer to send, by the name /mcp takes them by.
    let prompts = mcp::prompts();
    if rest.is_empty() && !prompts.is_empty() {
        render::said(&render::blue("●"), &render::bold("Prompts"));
        for (command, blurb) in &prompts {
            let prompt = command.strip_prefix("/mcp__").unwrap_or(command);
            let room = render::width().saturating_sub(prompt.chars().count() + 2);
            render::line(&format!(
                "{}  {}",
                render::bold(prompt),
                render::dim(&render::fit(blurb, room))
            ));
        }
        render::line(&render::dim("/mcp <prompt> to send one"));
    }
    render::line(&render::dim(
        "/mcp enable or /mcp disable to switch servers · /mcp reconnect|login|logout|log <name> · /mcp reload after editing mcp.json",
    ));
}

/// What picking a server for `/mcp <verb>` asks.
fn server_question(verb: &str) -> String {
    match verb {
        "reconnect" => "Reconnect which server?".to_owned(),
        "login" => "Sign in to which server?".to_owned(),
        "logout" => "Sign out of which server?".to_owned(),
        "log" => "Read which server's log?".to_owned(),
        _ => format!("Which server for /mcp {verb}?"),
    }
}

/// What picking a plugin for `/plugins <verb>` asks.
fn plugin_question(verb: &str) -> &'static str {
    match verb {
        "allow" => "Allow which plugin to start its programs?",
        _ => "Remove which plugin?",
    }
}

/// The MCP servers as a list to switch on and off with the arrows. False off
/// a keyboard, or with no servers to switch.
fn switch_servers(runner: &mut Runner) -> bool {
    use letronna_core::mcp::{self, State};
    let rows: Vec<_> = mcp::status()
        .iter()
        .map(|server| crate::bar::Switch {
            name: server.name.clone(),
            about: format!(
                "{} · {}",
                server.transport,
                render::plain(&crate::mcp::health(server))
            ),
            on: server.state != State::Disabled,
        })
        .collect();
    if rows.is_empty() {
        return false;
    }
    switch_list(runner, "MCP servers", rows, &mut |name, on| {
        mcp::set_enabled(name, on)
    })
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
    // Switching with no name, or the bare command, opens them as a list to
    // switch with the arrows; `/skills list` lists them as lines.
    let switching = rest.is_empty() || (matches!(verb, "on" | "off") && target.is_empty());
    if switching && !saved.is_empty() && switch_skills(runner, &saved) {
        return;
    }
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
    if !rest.is_empty() && rest != "list" {
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
        // A line each: descriptions are written for the model, and run long.
        let about = skill_about(skill);
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
        "/skills <name> to use one · /skills to switch them · /skills add <owner/repo, link or path>",
    ));
}

/// What a skill is listed with: where it came from, then its description.
fn skill_about(skill: &Skill) -> String {
    let source = match skill.source {
        Source::User if skill.learned => "learned",
        Source::User => "yours",
        Source::Project => "project",
        Source::Plugin => "plugin",
        Source::BuiltIn => "built in",
    };
    format!("{source} · {}", skill.description)
}

/// The skills as a list to switch on and off with the arrows. False off a
/// keyboard, where they are listed as lines instead.
fn switch_skills(runner: &mut Runner, saved: &[Skill]) -> bool {
    let rows = saved
        .iter()
        .map(|skill| crate::bar::Switch {
            name: skill.name.clone(),
            about: skill_about(skill),
            on: skill.enabled,
        })
        .collect();
    let switched = switch_list(runner, "Skills", rows, &mut |name, on| {
        letronna_core::skills::set_enabled(name, on)
    });
    if switched {
        render::line(&render::dim(
            "Only the ones on are offered to the model · /skills <name> to use one",
        ));
    }
    switched
}

/// `/tools`: Letronna's own tools in the families the window's Capabilities
/// page has, switched with the arrows, or by `on` or `off` and a family's
/// name; `/tools list`, or a run off a keyboard, lists them.
fn tools(runner: &mut Runner, rest: &str) {
    use letronna_core::toolset::{self, FAMILIES, Family};
    let (verb, target) = rest
        .split_once(char::is_whitespace)
        .map_or((rest, ""), |(verb, target)| (verb, target.trim()));
    // One that has no tools yet, as Plugins before a plugin adds some,
    // would switch nothing.
    let families = || FAMILIES.iter().filter(|family| !family.tools().is_empty());
    let rows = || {
        families()
            .map(|family| {
                let off = toolset::switched_off(&family.tools());
                family_row(*family, &off, family.readiness().map(|ready| ready.note))
            })
            .collect::<Vec<_>>()
    };
    let find = |name: &str| -> Result<Family, String> {
        let family = families().find(|family| family.title().eq_ignore_ascii_case(name));
        family.copied().ok_or_else(|| {
            let names: Vec<&str> = families().map(|family| family.title()).collect();
            format!("No family {name}; there are {}.", names.join(", "))
        })
    };
    let flip = &mut |name: &str, on: bool| toolset::switch(&find(name)?.tools(), on);
    render::blank();
    match verb {
        "" if switch_list(runner, "Tools", rows(), flip) => {}
        "on" | "off" if !target.is_empty() => match flip(target, verb == "on") {
            Ok(()) => {
                let name = find(target).map_or(target, |family| family.title());
                render::line(&render::dim(&format!("{name}: switched {verb}.")));
            }
            Err(why) => {
                render::line(&render::red(&why));
                return;
            }
        },
        "" | "list" => {
            let rows = rows();
            let wide = rows.iter().map(|row| row.name.len()).max().unwrap_or(0);
            for row in rows {
                let state = if row.on {
                    render::green("on ")
                } else {
                    render::dim("off")
                };
                let name = render::bold(&format!("{:wide$}", row.name));
                render::line(&format!("  {state}  {name}  {}", render::dim(&row.about)));
            }
        }
        _ => {
            render::line(&render::red(
                "/tools · /tools list · /tools on|off <family>",
            ));
            return;
        }
    }
    render::line(&render::dim(
        "For every session and helper; asking you and the to-do list stay on · \
         /tools on|off <family>",
    ));
}

/// A family as `/tools` lists it, with `off` its tools that are switched off
/// and `note` what it still needs: on unless every one of its tools is off,
/// as the window's switch has it.
fn family_row(
    family: letronna_core::toolset::Family,
    off: &[String],
    note: Option<&str>,
) -> crate::bar::Switch {
    let tools = family.tools();
    let off = tools.iter().filter(|name| off.contains(name)).count();
    let mut about = family.blurb().to_owned();
    if off > 0 && off < tools.len() {
        about.push_str(&format!(" · {off} of {} off", tools.len()));
    }
    if let Some(note) = note {
        about.push_str(&format!(" · {note}"));
    }
    crate::bar::Switch {
        name: family.title().to_owned(),
        about,
        on: off < tools.len(),
    }
}

/// A mode picked from those offered, each with what it does, the one in use
/// marked. `None` when it was turned down.
fn pick_mode(runner: &mut Runner) -> Option<&'static str> {
    let labels: Vec<String> = Mode::OFFERED
        .iter()
        .map(|mode| {
            let using = if *mode == runner.current_mode() {
                "  (in use)"
            } else {
                ""
            };
            format!("{}  ·  {}{using}", mode.label(), mode.about())
        })
        .collect();
    let mut options: Vec<&str> = labels.iter().map(String::as_str).collect();
    options.push("Keep the one I have");
    let picked = runner.bar.pick_from("Mode", Vec::new(), &options)?;
    Mode::OFFERED.get(picked).map(|mode| mode.name())
}

/// `/remember`: with words, a turn asking the model to keep them, in the
/// project's memory or this chat's notes after `--project` or `--chat`;
/// bare, when this chat saves memory and where it keeps it, picked from a
/// list on a keyboard.
fn remember(runner: &mut Runner, session: usize, rest: &str) -> Flow {
    let (flag, words) = rest.split_once(char::is_whitespace).unwrap_or((rest, ""));
    let (place, text) = match flag {
        "--project" => (Some("for this project"), words.trim()),
        "--chat" => (Some("for this chat only"), words.trim()),
        _ => (None, rest),
    };
    // Asked in the user's own words, so the turn counts as asking whatever
    // the chat chose, and a place named there holds for this save.
    if !text.is_empty() {
        return Flow::Send(format!("Remember {}: {text}", place.unwrap_or("this")));
    }
    render::blank();
    if place.is_some() {
        render::line(&render::red(&format!("Say what to remember after {flag}.")));
        return Flow::Handled;
    }
    if runner.bar.steered() {
        pick_remembering(runner, session);
    }
    let workspace = runner.store.conversations[session].workspace.is_some();
    let (saving, scope) = (
        runner.store.memory_saving(session),
        runner.store.memory_scope(session),
    );
    render::line(&render::dim(&format!(
        "This chat saves to memory {}, and keeps it {}.",
        saving.label().to_lowercase(),
        scope.label().to_lowercase()
    )));
    if scope == Scope::Project && !workspace {
        render::line(&render::dim(
            "With no project folder attached, that is this chat's own notes.",
        ));
    }
    if !runner.bar.steered() {
        render::line(&render::dim(
            "/remember <text> keeps something; --project or --chat before it keeps it there.",
        ));
    }
    Flow::Handled
}

/// Offers when this chat saves memory and where it keeps it, with the
/// chat's own choices marked, and does what was picked.
fn pick_remembering(runner: &mut Runner, session: usize) {
    /// What a row of the list does.
    enum Row {
        Private,
        Save(Option<Saving>),
        Keep(Option<Scope>),
        MakeDefault,
    }
    let conversation = &runner.store.conversations[session];
    let (own_saving, own_scope) = (conversation.memory_saving, conversation.memory_scope);
    let settings = &runner.store.settings;
    let marked = |text: String, chosen: bool| {
        if chosen {
            format!("{text}  (in use)")
        } else {
            text
        }
    };
    let mut rows = vec![(
        Row::Private,
        "Private chat  ·  only this chat's own notes, saved when you ask".to_owned(),
    )];
    for saving in Saving::OFFERED {
        let label = marked(
            format!("Save: {}", saving.label()),
            own_saving == Some(saving),
        );
        rows.push((Row::Save(Some(saving)), label));
    }
    let default = format!("Save: use my default ({})", settings.memory_saving.label());
    rows.push((Row::Save(None), marked(default, own_saving.is_none())));
    for scope in Scope::OFFERED {
        let label = marked(
            format!("Keep in: {}", scope.label()),
            own_scope == Some(scope),
        );
        rows.push((Row::Keep(Some(scope)), label));
    }
    let default = format!(
        "Keep in: use my default ({})",
        settings.memory_scope.label()
    );
    rows.push((Row::Keep(None), marked(default, own_scope.is_none())));
    rows.push((
        Row::MakeDefault,
        "Make this chat's choices the default for new chats".to_owned(),
    ));
    let mut options: Vec<&str> = rows.iter().map(|(_, label)| label.as_str()).collect();
    options.push("Leave them as they are");
    let picked = runner.bar.pick_from("Remember me", Vec::new(), &options);
    let Some((row, _)) = picked.and_then(|at| rows.get(at)) else {
        render::line(&render::dim("Left as it was."));
        return;
    };
    let store = &mut runner.store;
    match row {
        Row::Private => store.make_private(session),
        Row::Save(saving) => store.choose_memory_saving(session, *saving),
        Row::Keep(scope) => store.choose_memory_scope(session, *scope),
        Row::MakeDefault => {
            make_memory_default(store, session);
            render::line(&render::dim(
                "New chats start with these, and this chat now follows the default.",
            ));
        }
    }
    runner.persist();
}

/// Make the session's memory choices the default for new chats, then drop
/// its own so it follows the default, as the window's does; left set, they
/// would keep the chat apart from the default the user just changed.
fn make_memory_default(store: &mut store::Store, session: usize) {
    store.settings.memory_saving = store.memory_saving(session);
    store.settings.memory_scope = store.memory_scope(session);
    store.choose_memory_saving(session, None);
    store.choose_memory_scope(session, None);
}

/// A name picked in the box for a command given none, from `rows` of a name
/// and how it is listed, which typing narrows. `None` when it was turned
/// down, saying so; the caller asks only on a keyboard and with rows to show.
fn pick_name(runner: &mut Runner, question: &str, rows: &[(String, String)]) -> Option<String> {
    let mut options: Vec<&str> = rows.iter().map(|(_, label)| label.as_str()).collect();
    options.push("Never mind");
    let picked = runner.bar.pick_from(question, Vec::new(), &options);
    let name = picked
        .and_then(|at| rows.get(at))
        .map(|(name, _)| name.clone());
    if name.is_none() {
        render::line(&render::dim("Left as it was."));
    }
    name
}

/// Switch things on and off from a list in the box, and leave a line in the
/// conversation saying what changed. False off a keyboard, with nothing done.
fn switch_list(
    runner: &mut Runner,
    what: &str,
    rows: Vec<crate::bar::Switch>,
    flip: &mut dyn FnMut(&str, bool) -> Result<(), String>,
) -> bool {
    if !runner.bar.steered() {
        return false;
    }
    let before: Vec<bool> = rows.iter().map(|row| row.on).collect();
    let after = runner.bar.switches(what, rows, flip);
    let on = after.iter().filter(|row| row.on).count();
    render::said(
        &render::blue("●"),
        &render::bold(&format!("{what}: {on} of {} on", after.len())),
    );
    for (state, label) in [(true, "Switched on"), (false, "Switched off")] {
        let names: Vec<&str> = after
            .iter()
            .zip(&before)
            .filter(|(row, was)| row.on == state && row.on != **was)
            .map(|(row, _)| row.name.as_str())
            .collect();
        if !names.is_empty() {
            render::line(&render::dim(&format!("{label}: {}", names.join(", "))));
        }
    }
    if after.iter().zip(&before).all(|(row, was)| row.on == *was) {
        render::line(&render::dim("Nothing was switched."));
    }
    true
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
/// The plugins as a list to switch on and off with the arrows. False off a
/// keyboard, or with no plugins to switch.
fn switch_plugins(runner: &mut Runner) -> bool {
    use letronna_core::plugins;
    let rows: Vec<_> = match plugins::list() {
        Ok(found) => found
            .into_iter()
            .map(|plugin| crate::bar::Switch {
                about: match &plugin.broken {
                    Some(why) => format!("will not load · {why}"),
                    None => plugin.description.clone(),
                },
                name: plugin.name,
                on: plugin.enabled,
            })
            .collect(),
        Err(why) => {
            render::line(&render::red(&why));
            return true;
        }
    };
    if rows.is_empty() {
        return false;
    }
    switch_list(runner, "Plugins", rows, &mut |name, on| {
        plugins::set_enabled(name, on)
    })
}

/// `/wallet`: the verbs of `letronna wallet`, asked in the box; the
/// networks as a list of switches when there is a keyboard to turn them.
fn wallet(runner: &mut Runner, rest: &str) {
    let words: Vec<String> = rest.split_whitespace().map(str::to_owned).collect();
    if words.iter().map(String::as_str).eq(["chains"]) {
        let kept = letronna_core::wallet::load();
        let rows = letronna_core::wallet::networks(&kept)
            .into_iter()
            .map(|chain| crate::bar::Switch {
                about: if chain.testnet {
                    "test network".into()
                } else {
                    chain.id.clone()
                },
                name: chain.name,
                on: chain.on,
            })
            .collect();
        let mut flip = |name: &str, on: bool| {
            letronna_core::wallet::set_network(name, Some(on), None).map(|_| ())
        };
        if switch_list(runner, "Networks", rows, &mut flip) {
            return;
        }
    }
    render::blank();
    let done =
        crate::wallet::guard(&words).and_then(|()| crate::wallet::verb(&words, &mut runner.bar));
    match done {
        Ok(text) => {
            render::said(&render::blue("●"), &render::bold("Wallet"));
            for line in text.lines() {
                render::line(line);
            }
        }
        Err(why) => render::line(&render::red(&why)),
    }
}

fn plugins(runner: &mut Runner, rest: &str) {
    use letronna_core::plugins;
    render::blank();
    let (verb, target) = rest
        .split_once(' ')
        .map_or((rest, ""), |(verb, target)| (verb, target.trim()));
    let needs_name = matches!(
        verb,
        "on" | "off" | "allow" | "remove" | "add" | "settings" | "set"
    );
    if matches!(verb, "on" | "off") && target.is_empty() && switch_plugins(runner) {
        return;
    }
    // Allow and remove, with none named, offer the plugins they apply to.
    let offered: Vec<(String, String)> =
        match matches!(verb, "allow" | "remove") && target.is_empty() {
            true => plugins::list()
                .unwrap_or_default()
                .into_iter()
                .filter(|plugin| verb == "remove" || !plugin.allowed)
                .map(|plugin| {
                    let label = format!("{}  ·  {}", plugin.name, plugin.description);
                    (plugin.name, label)
                })
                .collect(),
            false => Vec::new(),
        };
    let picked = match runner.bar.steered() && !offered.is_empty() {
        true => match pick_name(runner, plugin_question(verb), &offered) {
            Some(plugin) => plugin,
            None => return,
        },
        false => target.to_owned(),
    };
    let target = picked.as_str();
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
        "settings" => return plugin_settings(target),
        "set" => return set_plugin_setting(target),
        "reload" => return reload_plugins(target),
        _ => Err(format!(
            "/plugins {verb} is not one of add, on, off, allow, update, remove, settings, set, \
             reload or market."
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
        "/plugins add <url|owner/repo> · on|off|allow|update|remove|settings <name> · reload · \
         market",
    ));
}

/// A plugin's settings with their values now, and how to change them.
fn plugin_settings(name: &str) {
    use letronna_core::plugins::code::{self, SettingKind};
    let settings = match code::settings(name) {
        Ok(settings) => settings,
        Err(why) => return render::line(&render::red(&why)),
    };
    render::said(
        &render::blue("●"),
        &render::bold(&format!("{name}'s settings")),
    );
    if settings.is_empty() {
        return render::line(&render::dim("It has none."));
    }
    for (setting, value) in &settings {
        let value = match (setting.kind, value) {
            (SettingKind::Switch, serde_json::Value::Bool(on)) => {
                if *on { "on" } else { "off" }.to_owned()
            }
            (_, serde_json::Value::String(text)) if text.is_empty() => "(empty)".to_owned(),
            (_, serde_json::Value::String(text)) => text.clone(),
            (_, other) => other.to_string(),
        };
        let choices = match setting.kind {
            SettingKind::Choice => format!("  ·  one of {}", setting.choices.join(", ")),
            _ => String::new(),
        };
        render::line(&format!(
            "{}  {}  {}",
            render::bold(&setting.key),
            value,
            render::dim(&format!("{}{choices}", setting.label))
        ));
    }
    render::line(&render::dim(&format!(
        "/plugins set {name} <key> <value> to change one; a switch takes on or off"
    )));
}

/// `/plugins set <name> <key> <value>`: keeps a plugin's setting.
fn set_plugin_setting(rest: &str) {
    use letronna_core::plugins::code::{self, SettingKind};
    let mut words = rest.splitn(3, char::is_whitespace);
    let (Some(name), Some(key)) = (words.next(), words.next()) else {
        return render::line(&render::red("Usage: /plugins set <name> <key> <value>"));
    };
    let given = words.next().unwrap_or_default().trim();
    let kind = code::settings(name)
        .ok()
        .and_then(|settings| settings.into_iter().find(|(setting, _)| setting.key == key))
        .map(|(setting, _)| setting.kind);
    let value = match (kind, given) {
        (Some(SettingKind::Switch), "on" | "yes" | "true") => serde_json::Value::Bool(true),
        (Some(SettingKind::Switch), "off" | "no" | "false") => serde_json::Value::Bool(false),
        _ => serde_json::Value::from(given),
    };
    match code::set_setting(name, key, value) {
        Ok(()) => render::line(&render::dim(&format!("{name}'s {key} is now {given}."))),
        Err(why) => render::line(&render::red(&why)),
    }
}

/// `/plugins reload [name]`: starts a plugin's code afresh, or every one's,
/// for an author who changed it in place.
fn reload_plugins(name: &str) {
    use letronna_core::plugins::code;
    code::reload();
    let names = match name {
        "" => (letronna_core::plugins::list()
            .unwrap_or_default()
            .into_iter())
        .filter(|plugin| plugin.code.is_some() && plugin.enabled && plugin.allowed)
        .map(|plugin| plugin.name)
        .collect(),
        name => vec![name.to_owned()],
    };
    if names.is_empty() {
        return render::line(&render::dim("No plugin runs code."));
    }
    for name in names {
        match code::restart(&name) {
            Ok(()) => render::line(&render::dim(&format!(
                "{name} starts afresh when it is next used."
            ))),
            Err(why) => render::line(&render::red(&why)),
        }
    }
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
    for (name, base) in &plugin.providers {
        render::line(&render::dim(&format!("     Provider {name}: {base}")));
    }
    if let Some(code) = &plugin.code {
        render::line(&render::dim(&format!("     {}", code.consent())));
        for brings in code.brings() {
            render::line(&render::dim(&format!("     {brings}")));
        }
        let runtime = letronna_core::plugins::code::Runtime::Python;
        if code.runtime == runtime && letronna_core::plugins::code::python_found() == Some(false) {
            render::line(&render::amber(
                "     It needs Python 3.9 or later, which was not found here; install it from \
                 python.org, or set LETRONNA_PYTHON to the one to use.",
            ));
        }
    }
    for why in &plugin.clashes {
        render::line(&render::amber(&format!("     {why}")));
    }
    if !plugin.allowed {
        let what = match (
            plugin.code.is_some(),
            plugin.servers.is_empty() && plugin.providers.is_empty(),
        ) {
            (true, true) => "Its code runs on this computer",
            (true, false) => {
                "Its code and MCP servers run on this computer and its providers are sent your \
                 messages"
            }
            (false, _) => {
                "Its MCP servers run programs on this computer and its providers are sent your \
                 messages"
            }
        };
        render::line(&render::amber(&format!(
            "     {what}, so it waits for your say: /plugins allow {} to use it.",
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

/// Sessions offered in the box, where typing narrows them by title, so the
/// list reaches further back than the shortlist.
const SESSION_LIST: usize = 50;

/// How a session reads in the `/resume` list: its title, project folder and
/// length.
fn choice_label(conversation: &store::Conversation) -> String {
    // On one line: a name taken from a prompt's first lines would break the row.
    let title = match render::one_line(&conversation.title) {
        title if title.is_empty() => "untitled".to_owned(),
        title => title,
    };
    let folder = conversation
        .workspace
        .as_ref()
        .and_then(|p| p.file_name())
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "no project".into());
    // Listed sessions are not read yet, so their messages are empty and the
    // count comes from the listing.
    let away = match letronna_core::background::running(conversation.id) {
        true => "  ·  working in the background",
        false => "",
    };
    format!(
        "{title}  ·  {folder}  ·  {} messages{away}",
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
    // Scheduled runs, and the window, make sessions while this is open.
    runner.store.pick_up_new();
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
    recent.truncate(if runner.bar.steered() {
        SESSION_LIST
    } else {
        SESSION_CHOICES
    });

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
    let Some(picked) = runner.bar.pick_from("Which one?", Vec::new(), &options) else {
        render::line(&render::dim("Stayed put."));
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
    let id = runner.store.conversations[index].id;
    if letronna_core::background::running(id) {
        render::blank();
        render::line(&render::dim(&crate::background::busy(id)));
        return;
    }
    *session = index;
    runner.enter(index);
    // A session carries its own project folder; one that never had a folder
    // takes the one this window is standing in.
    if runner.store.conversations[index].workspace.is_none() {
        runner.store.conversations[index].workspace = workspace.map(|p| p.to_path_buf());
    }
    // Made again if it went, so the session works where it did.
    if let Err(error) = letronna_core::worktree::resume(&mut runner.store.conversations[index]) {
        render::line(&render::red(&error));
    }
    runner.persist();

    // No title line here: the first thing replayed is the message the title
    // was taken from, and saying it twice reads as the command having run
    // twice.
    replay(runner, index);
    runner.recap_if_due(index);
    // Notices that came for it while another session was open rang for that
    // one, so they are taken now; and a turn stopped over there is no reason
    // for this one to stay quiet.
    runner.stopped = false;
    if let Err(error) = runner.resume(index) {
        runner.show_failure(&error);
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

/// Whether /rewind or /undo was given what it takes: nothing, or the
/// `--files` /undo once needed to put files back, which is now a choice.
fn rewinds(rest: &str) -> bool {
    matches!(rest, "" | "--files")
}

/// What going back to a message can do, offered once it is picked.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Back {
    /// The conversation goes back to before it, and the files with it.
    Both,
    /// The conversation goes back; the files stay as they are now.
    Conversation,
    /// The files go back; the conversation stays.
    Code,
    /// What came before it is summarized; nothing goes back.
    Summary,
    /// A copy of the session carries on from before it.
    Branch,
    /// Back to the list.
    NeverMind,
}

impl Back {
    /// The answer as the question offers it, in Claude Code's words where
    /// it has them.
    fn label(self) -> &'static str {
        match self {
            Back::Both => "Restore code and conversation",
            Back::Conversation => "Restore conversation",
            Back::Code => "Restore code",
            Back::Summary => "Summarize up to here",
            Back::Branch => "Branch from here",
            Back::NeverMind => "Never mind",
        }
    }
}

/// What going back to a message offers: the code only where a file can go
/// back, and a summary only where something before it is not summarized.
fn offered(code: bool, summarizes: bool) -> Vec<Back> {
    let mut offered = Vec::new();
    if code {
        offered.push(Back::Both);
    }
    offered.push(Back::Conversation);
    if code {
        offered.push(Back::Code);
    }
    if summarizes {
        offered.push(Back::Summary);
    }
    offered.extend([Back::Branch, Back::NeverMind]);
    offered
}

/// Go back to a message you sent, picked from the newest down. Never mind,
/// or escape, on what to do there comes back to the list.
fn rewind(runner: &mut Runner, session: &mut usize, workspace: Option<&std::path::Path>) {
    loop {
        let messages = &runner.store.conversations[*session].messages;
        let points = rewind_points(messages);
        if points.is_empty() {
            render::blank();
            render::line(&render::dim("Nothing to go back to yet."));
            return;
        }
        let room = render::width().saturating_sub(PICKER_EDGES);
        let rows: Vec<String> = (points.iter())
            .map(|&at| point_label(messages, at, room))
            .collect();
        let options: Vec<&str> = rows.iter().map(String::as_str).collect();
        let detail = vec!["Your messages, the newest first; type to narrow them.".to_owned()];
        let picked = runner
            .bar
            .pick_from("Go back to before which message?", detail, &options);
        let Some(picked) = picked else {
            return;
        };
        if go_back(runner, session, workspace, points[picked]) {
            return;
        }
    }
}

/// Where each message you sent that started a turn sits, the newest first:
/// the places a rewind goes back to. A notice is Letronna's, not yours.
fn rewind_points(messages: &[Message]) -> Vec<usize> {
    turn_starts(messages)
        .rev()
        .filter(|&at| !messages[at].notice)
        .collect()
}

/// A message as the rewind lists it: what you said, on one row, and how
/// many files its turn changed, cut to `room` columns with the count kept.
fn point_label(messages: &[Message], at: usize, room: usize) -> String {
    let message = &messages[at];
    let said = match bar::as_typed(&message.content) {
        said if !said.trim().is_empty() => said,
        _ if !message.images.is_empty() => "(a picture)".to_owned(),
        _ => "(nothing)".to_owned(),
    };
    let files = match turn_files(messages, at) {
        0 => String::new(),
        1 => "  ·  1 file".to_owned(),
        n => format!("  ·  {n} files"),
    };
    // A pasted prompt can hold an escape, which printed raw would restyle
    // or move about the terminal.
    let said: String = render::fit(&said, room.saturating_sub(files.chars().count()))
        .chars()
        .filter(|c| !c.is_control())
        .collect();
    format!("{said}{files}")
}

/// How many files the turn your message at `at` started changed, as its
/// tools and commands noted them: read from the messages alone, so a long
/// session lists at once.
fn turn_files(messages: &[Message], at: usize) -> usize {
    let end = turn_starts(messages)
        .find(|&start| start > at && !messages[start].notice)
        .unwrap_or(messages.len());
    let mut paths: Vec<&std::path::Path> = Vec::new();
    let changes = (messages[at..end].iter())
        .filter(|m| m.role == "tool")
        .flat_map(|m| &m.files);
    for change in changes {
        if !paths.contains(&change.path.as_path()) {
            paths.push(&change.path);
        }
    }
    paths.len()
}

/// Characters of your message the question quotes, enough to know it by.
const QUOTED: usize = 60;

/// Ask what to do at your message `at`, and do it. `false` when the answer
/// was Never mind, or escape, so another message can be picked.
fn go_back(
    runner: &mut Runner,
    session: &mut usize,
    workspace: Option<&std::path::Path>,
    at: usize,
) -> bool {
    let conversation = &runner.store.conversations[*session];
    let id = conversation.id;
    // Without a project the tools wrote in the fallback folder, which the
    // rewind finds itself, not in the folder the terminal started in.
    let folder = conversation.workspace.clone();
    let messages = conversation.messages.clone();
    let summarizes = at > conversation.floor();
    let (dropped, looked) = (messages[at..].to_vec(), folder.clone());
    // Each file the dropped turns wrote is read and hashed, which a big one
    // makes slow.
    let plan = runner.bar.busy("reading what changed since", move || {
        history::plan(id, &dropped, looked.as_deref())
    });
    let dropped = &messages[at..];
    // What stays, and why, said after the files.
    let commands: Vec<String> = match history::ran_commands(dropped) {
        true => std::iter::once(history::COMMANDS_NOTE.to_owned())
            .chain(history::unkept(dropped))
            .collect(),
        false => Vec::new(),
    };
    let code = plan.iter().any(|file| file.fate.touches());
    let said: String = render::fit(&bar::as_typed(&messages[at].content), QUOTED)
        .chars()
        .filter(|c| !c.is_control())
        .collect();
    let mut detail = vec![format!("“{said}”"), leaving(dropped.len())];
    detail.extend(history::listing(&plan, folder.as_deref()));
    detail.extend(commands.iter().cloned());
    let offered = offered(code, summarizes);
    let labels: Vec<&str> = offered.iter().map(|back| back.label()).collect();
    let picked = runner
        .bar
        .choose("Go back to before this message?", detail, &labels);
    let Some(back) = picked.map(|picked| offered[picked]) else {
        return false;
    };
    match back {
        Back::NeverMind => return false,
        Back::Both | Back::Conversation => {
            let prompt = take_back_to(runner, *session, workspace, at);
            runner.bar.say(&prompt);
            render::blank();
            render::line(&render::dim(&format!(
                "Went back to before “{said}”. Your message is back in the box; /redo puts \
                 it all back."
            )));
            let mut undone = Undone {
                session: id,
                left: at,
                messages: Some(dropped.to_vec()),
                files: None,
                said: said.clone(),
            };
            if back == Back::Conversation {
                if let Some(stay) = staying(&plan, folder.as_deref()) {
                    render::line(&render::dim(&stay));
                }
                keep_undone(undone);
                return true;
            }
            // The question went with the redraw, so what was left alone is
            // said again here.
            let restored = history::restore(id, &plan, folder.as_deref());
            for line in restored.lines() {
                render::line(&render::dim(&line));
            }
            for line in &commands {
                render::line(&render::dim(line));
            }
            undone.files = restored.notice();
            keep_undone(undone);
        }
        Back::Code => {
            let restored = history::restore(id, &plan, folder.as_deref());
            render::blank();
            for line in restored.lines() {
                render::line(&render::dim(&line));
            }
            for line in &commands {
                render::line(&render::dim(line));
            }
            // The conversation still holds the edits, which the model would
            // otherwise take for what is on disk.
            if let Some(notice) = restored.notice() {
                let messages = &mut runner.store.conversations[*session].messages;
                messages.push(notice.clone());
                let left = messages.len();
                runner.persist();
                render::line(&render::dim("/redo puts the agent's versions back."));
                keep_undone(Undone {
                    session: id,
                    left,
                    messages: None,
                    files: Some(notice),
                    said: said.clone(),
                });
            }
        }
        Back::Summary => {
            let keep = turns_from(&messages, at);
            compact(runner, *session, &keep.to_string());
        }
        Back::Branch => {
            let from = runner.store.conversations[*session].id;
            branch(runner, session, "");
            if runner.store.conversations[*session].id == from {
                return true;
            }
            // The copy stops before the message, with its text in the box to
            // be asked another way; the original keeps what came after.
            let prompt = take_back_to(runner, *session, workspace, at);
            runner.bar.say(&prompt);
            render::blank();
            render::line(&render::dim(&format!(
                "Now in {}, from before “{said}”. The original is kept; /resume goes back to it.",
                runner.store.conversations[*session].title
            )));
        }
    }
    true
}

/// What a rewind took back, for `/redo` to put back.
struct Undone {
    /// The session it was in.
    session: u64,
    /// How many messages it left there, which is all there may be for it
    /// to be put back: anything said since was said without it.
    left: usize,
    /// The messages it took out of the chat; `None` when only the code went
    /// back, whose notice is the last of `left` and goes again.
    messages: Option<Vec<Message>>,
    /// The notice of the files it put back, which a rewind past it would
    /// undo, and so does `/redo`.
    files: Option<Message>,
    /// The start of the message it went back to before, as it said it.
    said: String,
}

/// The rewinds `/redo` can put back, the latest last: each one after
/// another, for as long as nothing new is said.
static UNDONE: std::sync::Mutex<Vec<Undone>> = std::sync::Mutex::new(Vec::new());

/// The rewinds kept, whole even after a panic while they were held.
fn undone() -> std::sync::MutexGuard<'static, Vec<Undone>> {
    UNDONE.lock().unwrap_or_else(|e| e.into_inner())
}

/// Keep `latest` for `/redo`. A rewind of another session starts the list
/// over, since `/redo` only reaches the one it is in.
fn keep_undone(latest: Undone) {
    let mut kept = undone();
    kept.retain(|undone| undone.session == latest.session);
    kept.push(latest);
}

/// The latest rewind of session `id`, which holds `now` messages, taken off
/// the list to be put back, or why there is none to put back.
fn redoable(id: u64, now: usize) -> Result<Undone, &'static str> {
    let mut kept = undone();
    let Some(last) = kept.pop_if(|undone| undone.session == id) else {
        return Err("Nothing to redo: /redo puts back what /undo or /rewind last took back.");
    };
    if last.left != now {
        // What was said since was said without them, and every rewind
        // before it was one more back.
        kept.clear();
        return Err(
            "Not redone: something was said since /undo, without what it took \
                    back. /rewind can still go back to any message.",
        );
    }
    Ok(last)
}

/// Put back what the latest rewind in this session took back: its messages
/// and the files as the agent had left them, unless something was said
/// since, which would have been said without them.
fn redo(runner: &mut Runner, session: usize, workspace: Option<&std::path::Path>) {
    render::blank();
    let conversation = &runner.store.conversations[session];
    let (id, now) = (conversation.id, conversation.messages.len());
    let last = match redoable(id, now) {
        Ok(last) => last,
        Err(why) => return render::line(&render::dim(why)),
    };
    let folder = conversation.workspace.clone();
    let mut files = Vec::new();
    if let Some(notice) = &last.files {
        let plan = history::plan(id, std::slice::from_ref(notice), folder.as_deref());
        let restored = history::restore(id, &plan, folder.as_deref());
        match restored.back {
            0 => {}
            1 => files.push("1 file is as the agent left it again.".to_owned()),
            n => files.push(format!("{n} files are as the agent left them again.")),
        }
        files.extend(restored.left);
    }
    let say = |files: &[String]| {
        for line in files {
            render::line(&render::dim(line));
        }
    };
    let Some(messages) = last.messages else {
        // Only the code went back, and its notice is all there is to take
        // out again.
        runner.store.conversations[session].rewind(now.saturating_sub(1));
        runner.persist();
        return say(&files);
    };
    // The pictures of the message taken back went into the box with it,
    // and come back with it instead.
    if messages
        .first()
        .is_some_and(|first| first.images == runner.images)
    {
        runner.images.clear();
    }
    let conversation = &mut runner.store.conversations[session];
    conversation.messages.extend(messages);
    conversation.todos = store::todos_in(&conversation.messages);
    runner.bar.set_plan(&conversation.todos);
    // What came back is drawn where it was, as the rewind drew what was left.
    render::clear();
    banner(runner, &folder_name(workspace), session);
    replay(runner, session);
    runner.persist();
    render::blank();
    render::line(&render::dim(&format!(
        "Redone: “{}” and what came after it are back.",
        last.said
    )));
    // After the redraw, which would take them off the screen.
    say(&files);
}

/// What restoring the conversation takes out of the chat, `gone` messages
/// from yours on, and what comes back to the box.
fn leaving(gone: usize) -> String {
    match gone {
        0 | 1 => "Restoring the conversation removes this message from the chat, and its text \
                  goes back in the box."
            .to_owned(),
        2 => "Restoring the conversation removes this message and the 1 message after it from \
              the chat, and its text goes back in the box."
            .to_owned(),
        n => format!(
            "Restoring the conversation removes this message and the {} messages after it \
             from the chat, and its text goes back in the box.",
            n - 1
        ),
    }
}

/// The files a rewind of the conversation alone leaves as they are, said
/// after it; `None` when it would have put none back anyway.
fn staying(plan: &[history::Restore], folder: Option<&std::path::Path>) -> Option<String> {
    let names: Vec<String> = (plan.iter())
        .filter(|file| file.fate.touches())
        .map(|file| history::label(&file.path, folder))
        .collect();
    let shown: Vec<&str> = names.iter().take(STAYING).map(String::as_str).collect();
    let shown = shown.join(", ");
    match names.len() {
        0 => None,
        n if n > STAYING => Some(format!(
            "The files it changed stay as they are: {shown} and {} more.",
            n - STAYING
        )),
        _ => Some(format!("The files it changed stay as they are: {shown}.")),
    }
}

/// Files a rewind of the conversation alone names as staying; past this it
/// says how many more, as the question does.
const STAYING: usize = 10;

/// How many of the session's turns start at your message `at` or after it:
/// what `/compact` keeps word for word, so its summary stops just before.
fn turns_from(messages: &[Message], at: usize) -> usize {
    turn_starts(messages).filter(|&start| start >= at).count()
}

/// Summarize the session now, all but its last `rest` turns, the latest one
/// when no number is given, and say how much smaller the next request is.
fn compact(runner: &mut Runner, session: usize, rest: &str) {
    let keep = match rest {
        "" => Some(1),
        n => n.parse::<usize>().ok().filter(|n| *n > 0),
    };
    render::blank();
    let Some(keep) = keep else {
        render::line(&render::red(
            "Usage: /compact, or /compact 3 to keep the last three turns word for word.",
        ));
        return;
    };
    let conversation = &runner.store.conversations[session];
    let messages = conversation.messages.clone();
    let upto = turn_starts(&messages).nth_back(keep - 1).unwrap_or(0);
    let floor = conversation.floor();
    let summary = conversation.summary.as_ref().map(|(_, text)| text.clone());
    let connection = runner.connection(session);
    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let stopped = stop.clone();
    let label = "summarizing the earlier turns · esc to stop";
    let done = runner.bar.busy_cancellable(label, stop, move || {
        let (sender, receiver) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let done = chat::compact(&connection, &messages, floor, summary.as_deref(), upto);
            let _ = sender.send(done);
        });
        // A request cannot hear a stop, so it is left to end on its own and
        // what it makes goes unused.
        loop {
            match receiver.recv_timeout(std::time::Duration::from_millis(100)) {
                Ok(done) => return done,
                Err(std::sync::mpsc::RecvTimeoutError::Timeout)
                    if !stopped.load(Ordering::Relaxed) => {}
                Err(_) => return Err("Stopped, so every turn is kept as it was.".into()),
            }
        }
    });
    let done = match done {
        Ok(done) => done,
        Err(why) => {
            render::line(&render::red(&why));
            return;
        }
    };
    let conversation = &mut runner.store.conversations[session];
    let kept = turn_starts(&conversation.messages)
        .filter(|at| *at >= done.upto)
        .count();
    let report = done.report();
    if let Err(why) = conversation.compacted(done) {
        render::line(&render::red(&why));
        return;
    }
    runner.persist();
    let kept = match kept {
        1 => "The latest turn stays".to_owned(),
        n => format!("The last {n} turns stay"),
    };
    render::line(&render::dim(&format!("{report}. {kept} word for word.")));
}

/// Where each of the session's turns starts.
fn turn_starts(messages: &[Message]) -> impl DoubleEndedIterator<Item = usize> + '_ {
    messages
        .iter()
        .enumerate()
        .filter(|(_, m)| chat::opens_turn(m))
        .map(|(at, _)| at)
}

/// Take the session back to just before the last thing you said, and the
/// screen with it. Returns what you said -- a command as it was typed -- or
/// `None` when nothing has been.
fn take_back(
    runner: &mut Runner,
    session: usize,
    workspace: Option<&std::path::Path>,
) -> Option<String> {
    let (at, _) = last_prompt(&runner.store.conversations[session].messages)?;
    Some(take_back_to(runner, session, workspace, at))
}

/// Take the session back to just before your message at `at`, and the
/// screen with it. Returns what you said, as it was typed: a command as
/// the command, and a message that began with a slash with the slash
/// doubled again, so sending it again does not run it.
fn take_back_to(
    runner: &mut Runner,
    session: usize,
    workspace: Option<&std::path::Path>,
    at: usize,
) -> String {
    let prompt = runner.store.conversations[session].messages[at]
        .content
        .clone();
    // A review of what is taken back must not keep any of it.
    letronna_core::review::stop(&runner.review);
    let conversation = &mut runner.store.conversations[session];
    // Its pictures come back with its words, to go with them again.
    runner.images = std::mem::take(&mut conversation.messages[at].images);
    runner.bar.set_attached(runner.images.len());
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
    bar::as_typed(&prompt)
}

/// Attach the picture at `raw` to the next message, saved as the window
/// saves one pasted there; with no path, say what is attached, and with
/// `clear`, drop it.
fn attach(runner: &mut Runner, raw: &str, workspace: Option<&std::path::Path>) {
    render::blank();
    if raw == "clear" {
        runner.images.clear();
        runner.bar.set_attached(0);
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
    // A picture taken off the clipboard was only waiting for this: the copy
    // just kept with the media is the one that goes, and one is enough.
    let pasted = crate::picture::pasted(&path);
    match saved {
        Ok(saved) => {
            if pasted {
                let _ = std::fs::remove_file(&path);
            }
            runner.images.push(saved);
            runner.bar.set_attached(runner.images.len());
            let what = if pasted {
                "the picture from the clipboard".to_owned()
            } else {
                path.display().to_string()
            };
            render::line(&render::dim(&format!(
                "Attached {what}; it goes with your next message."
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
    let notes = copy_session(runner, session, title.clone());
    letronna_core::review::stop(&runner.review);
    runner.show_goal();
    render::blank();
    render::line(&render::dim(&format!(
        "Now in {title}. The original is kept; /resume goes back to it."
    )));
    if let Err(error) = notes {
        render::line(&render::red(&error));
    }
}

/// Make a copy of the session at `session` called `title`, its chat notes
/// and summaries with it, and open the copy there, the original kept as it
/// was. Copying the notes is the error, the copy being made either way.
pub(crate) fn copy_session(
    runner: &mut Runner,
    session: &mut usize,
    title: String,
) -> Result<(), String> {
    let from = &runner.store.conversations[*session];
    let mut copy = store::Conversation {
        id: runner.store.reserve_id(),
        title,
        messages: from.messages.clone(),
        updated: letronna_core::chat::now(),
        workspace: from.workspace.clone(),
        todos: from.todos.clone(),
        model: from.model.clone(),
        gateway: from.gateway.clone(),
        effort: from.effort.clone(),
        // A private chat's copy is private too, and knows what it knew.
        memory_saving: from.memory_saving,
        memory_scope: from.memory_scope,
        ..Default::default()
    };
    copy.copy_summaries(from);
    history::share(from.id, copy.id);
    let notes = letronna_core::memory::copy_chat_notes(from.id, copy.id);
    runner.store.conversations.push(copy);
    *session = runner.store.conversations.len() - 1;
    runner.store.active = *session;
    runner.persist();
    notes
}

/// Models listed at once where they cannot be steered through. Some gateways
/// carry hundreds; there, a list longer than this is narrowed with a word. In
/// the box they are all offered, and narrowed by typing.
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
    let room = if runner.bar.steered() {
        offers.len()
    } else {
        MODEL_CHOICES
    };
    let shown = &offers[..offers.len().min(room)];
    let mut labels = vec!["Auto · the cheapest working".to_owned()];
    labels.extend(shown.iter().map(|o| format!("{} · {}", o.id, o.price())));
    let mut options: Vec<&str> = labels.iter().map(String::as_str).collect();
    options.push("Keep the one I have");
    let question = format!("In use: {}", chosen.as_deref().unwrap_or("Auto"));
    let Some(picked) = runner.bar.pick_from(&question, Vec::new(), &options) else {
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
    pick_model(runner, session, filter, false);
}

/// `/model`, or (`in_setup`) the setup's own model step, which has its own
/// heading and is already where a provider gets added.
pub fn pick_model(runner: &mut Runner, session: Option<usize>, filter: &str, in_setup: bool) {
    // `/model image`, `/model video`: what makes those, not the chat model.
    if let Some((kind, label)) = media_kind(filter) {
        switch_media(runner, kind, label);
        return;
    }
    let keys = store::load_keys();
    // The window may have moved the Custom address since this one read it.
    runner.store.follow_custom();
    let custom = runner.store.settings.custom_endpoint.clone();
    let settings = &runner.store.settings;
    let (gateway, model) = match session {
        Some(index) => runner.store.model(index),
        None => (settings.gateway.as_str(), settings.model.as_str()),
    };
    let current = format!("{model} · {gateway}");
    let (gateway, model) = (gateway.to_owned(), model.to_owned());

    if !in_setup {
        render::blank();
        render::said(&render::blue("●"), &render::bold("Models"));
    }
    // Fetching takes a moment over the network; say so rather than freezing.
    // An account whose hosted models need a subscription is checked again
    // beside it, as it may have been bought since the file was written.
    let check = letronna_core::auth::account().is_some_and(|a| a.needs_upgrade());
    let (models, errors, account) =
        runner
            .bar
            .busy("asking every gateway what it has", move || {
                let fresh = check.then(|| std::thread::spawn(letronna_core::auth::refresh));
                let (models, errors) = letronna_core::gateway::all_models(&keys, &custom);
                if let Some(fresh) = fresh {
                    let _ = fresh.join();
                }
                (models, errors, letronna_core::auth::account())
            });
    let unpaid =
        |g: &str, m: &str| letronna_core::gateway::needs_subscription(g, m, account.as_ref());
    let access = |g: &str, m: &str| {
        use letronna_core::gateway;
        let account = account.as_ref();
        match unpaid(g, m) {
            true if gateway::used_up_today(g, m, account) => Access::UsedUp,
            true => Access::Unpaid,
            false if gateway::free_today(g, m, account) => Access::Free,
            false => Access::Open,
        }
    };
    // A model on several accounts of one provider is listed once.
    let mut models = letronna_core::gateway::one_per_provider(models, (&gateway, &model));
    for error in &errors {
        render::line(&render::red(error));
    }
    if models.is_empty() {
        if errors.is_empty() {
            render::line(&render::dim(if in_setup {
                "Nothing offered any models yet."
            } else {
                "No gateway offered any models. Sign in with /login, or add a provider with `letronna setup`."
            }));
        }
        return;
    }
    // Those that need a subscription last, so what can answer comes first.
    models.sort_by(|a, b| {
        (unpaid(&a.0, &a.1).cmp(&unpaid(&b.0, &b.1)))
            .then(a.1.cmp(&b.1))
            .then(a.0.cmp(&b.0))
    });
    let usable: Vec<(String, String)> = (models.iter())
        .filter(|(g, m)| !unpaid(g, m))
        .cloned()
        .collect();

    // An exact name is an instruction, not a search: on a gateway that can
    // answer it when one has it, and refused when only the hosted pool has
    // it and it cannot be paid for.
    let exact = match exactly(&usable, filter, &gateway) {
        Ok((gateway, model)) => {
            set_model(runner, session, gateway, model);
            return;
        }
        Err(several) if !several.is_empty() => several,
        Err(_) => match exactly(&models, filter, &gateway) {
            Ok((g, m)) => {
                refuse_unpaid(&usable, access(&g, &m) == Access::UsedUp);
                return;
            }
            Err(several) => several,
        },
    };

    let needle = filter.to_lowercase();
    let mut matching: Vec<(String, String)> = match exact.is_empty() {
        false => exact,
        true => models
            .iter()
            .filter(|(g, m)| {
                needle.is_empty()
                    || m.to_lowercase().contains(&needle)
                    || g.to_lowercase().contains(&needle)
            })
            .cloned()
            .collect(),
    };
    // The one in use leads, so keeping it is one key away, unless it cannot
    // be paid for.
    if let Some(at) = matching
        .iter()
        .position(|(g, m)| *g == gateway && *m == model && !unpaid(g, m))
    {
        let current = matching.remove(at);
        matching.insert(0, current);
    }
    if matching.is_empty() {
        render::line(&render::dim(&format!(
            "Nothing matching \"{filter}\" among {} models.",
            models.len()
        )));
        return;
    }

    let room = if runner.bar.steered() {
        matching.len()
    } else {
        MODEL_CHOICES
    };
    let shown: Vec<(String, String)> = matching.iter().take(room).cloned().collect();
    if matching.len() > shown.len() {
        render::line(&render::dim(&format!(
            "{} of {} shown — type /model <word> to narrow it.",
            shown.len(),
            matching.len()
        )));
    }
    let labels: Vec<String> = shown
        .iter()
        .map(|(g, m)| model_label(m, g, *g == gateway && *m == model, access(g, m)))
        .collect();
    let mut options: Vec<&str> = labels.iter().map(String::as_str).collect();
    options.push("Keep the one I have");

    let question = format!("In use: {current}");
    // Said over the list, so the models marked are not a surprise once picked.
    let detail = match shown.iter().any(|(g, m)| unpaid(g, m)) {
        true => vec![format!(
            "Hosted models need a subscription on the Free plan: {}",
            letronna_core::auth::checkout()
        )],
        false => Vec::new(),
    };
    let Some(picked) = runner.bar.pick_from(&question, detail, &options) else {
        render::line(&render::dim("Left as it was."));
        return;
    };
    match shown.get(picked) {
        Some((gateway, model)) if unpaid(gateway, model) => {
            refuse_unpaid(&usable, access(gateway, model) == Access::UsedUp)
        }
        Some((gateway, model)) => set_model(runner, session, gateway.clone(), model.clone()),
        None => render::line(&render::dim("Left as it was.")),
    }
}

/// Where a model stands with the account signed in, as `/model` marks it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Access {
    /// Paid for as any other.
    Open,
    /// A hosted one a perk makes free today.
    Free,
    /// A hosted one the account has no plan or credit for.
    Unpaid,
    /// A hosted one free on a perk whose allowance is used up today.
    UsedUp,
}

/// A model as `/model` lists it: the one in use said, and one free today or
/// that the account cannot pay for marked.
fn model_label(model: &str, gateway: &str, in_use: bool, access: Access) -> String {
    let marks = [
        in_use.then_some("in use"),
        match access {
            Access::Open => None,
            Access::Free => Some("free today"),
            Access::Unpaid => Some("needs a subscription"),
            Access::UsedUp => Some("free · used up today"),
        },
    ];
    let marks: Vec<&str> = marks.into_iter().flatten().collect();
    match marks.is_empty() {
        true => format!("{model} · {gateway}"),
        false => format!("{model} · {gateway} ({})", marks.join(" · ")),
    }
}

/// Say why a hosted model was not switched to, with `usable` the models
/// that can answer instead; `used_up` when it is free on a perk whose
/// allowance for the day is used up.
fn refuse_unpaid(usable: &[(String, String)], used_up: bool) {
    let pages = [
        letronna_core::auth::checkout(),
        letronna_core::auth::credit_checkout(),
    ];
    let [why, instead] = unpaid_lines(
        letronna_core::gateway::usable_elsewhere(usable),
        used_up,
        &pages,
    );
    render::blank();
    render::line(&render::amber(&why));
    render::line(&render::dim(&instead));
}

/// How long the terminal's start waits on the account service before it
/// says the session's model is closed from what the file kept.
const HINT_CHECK: std::time::Duration = std::time::Duration::from_secs(2);

/// One line for a session on hosted models the account cannot pay for, as
/// the terminal starts: its next message would only be refused. `ready` is
/// the gateways set up besides, as `gateway::ready` names them.
pub fn unpaid_hint(gateway: &str, model: &str, ready: &[&str]) -> Option<String> {
    use letronna_core::auth;
    let locked = |account: Option<&auth::Account>| {
        letronna_core::gateway::needs_subscription(gateway, model, account)
    };
    let mut account = auth::account();
    if !locked(account.as_ref()) {
        return None;
    }
    // What the file says is as of the last check, so the API is asked
    // again, briefly, before the model is said to be closed: a plan may have
    // been bought since, or today's perks may make it free.
    if let Ok(fresh) = auth::refresh_within(HINT_CHECK) {
        letronna_core::gateway::learn_maker(model, &fresh);
        account = Some(fresh);
    }
    if !locked(account.as_ref()) {
        return None;
    }
    let why = match letronna_core::gateway::used_up_today(gateway, model, account.as_ref()) {
        true => "This session's model has used up its free allowance for today, back at 00:00 UTC",
        false => "Letronna's hosted models need a subscription",
    };
    Some(format!(
        "{why}, so this session's messages would be refused. {}",
        unpaid_instead(ready, &letronna_core::auth::checkout())
    ))
}

/// Whether a message to `session` is held back, and said why: its model is
/// a hosted one the account cannot pay for, even once the API is asked
/// again, as the plan may have been bought since the file was written, or
/// a perk's allowance may be back.
pub fn hold_unpaid(runner: &mut Runner, session: usize) -> bool {
    use letronna_core::{auth, gateway};
    let (on, model) = runner.store.model(session);
    let (on, model) = (on.to_owned(), model.to_owned());
    if !gateway::needs_subscription(&on, &model, auth::account().as_ref()) {
        return false;
    }
    let asked = model.clone();
    let fresh = runner.bar.busy("checking your plan", move || {
        let fresh = auth::refresh();
        // A perk for one maker's models needs to know who made this one.
        if let Ok(account) = &fresh {
            gateway::learn_maker(&asked, account);
        }
        fresh
    });
    let account = fresh.ok().or_else(auth::account);
    if !gateway::needs_subscription(&on, &model, account.as_ref()) {
        return false;
    }
    let keys = store::load_keys();
    let ready = gateway::ready(&keys, &runner.store.settings.custom_endpoint);
    let ready: Vec<&str> = ready.iter().map(|(name, ..)| *name).collect();
    let used_up = gateway::used_up_today(&on, &model, account.as_ref());
    render::blank();
    render::line(&render::amber(&held_line(
        used_up,
        &unpaid_instead(&ready, &auth::checkout()),
    )));
    render::line(&render::dim("↑ brings the message back."));
    true
}

/// Why a message was held back, then `instead`: the hosted models need a
/// subscription, or this one's free allowance is used up for the day.
fn held_line(used_up: bool, instead: &str) -> String {
    match used_up {
        true => format!(
            "Not sent: this model's free allowance is used up today and comes back at 00:00 UTC. {instead}"
        ),
        false => format!("Not sent: Letronna's hosted models need a subscription. {instead}"),
    }
}

/// Where to go instead of the hosted models: the first gateway in `ready`
/// that can answer, or the page an upgrade is bought on.
fn unpaid_instead(ready: &[&str], checkout: &str) -> String {
    use letronna_core::providers::{self, AuthKind};
    // Ollama is listed whether or not it runs, so it is not pointed to.
    let other = (ready.iter()).find(|name| {
        !matches!(
            providers::find(name).map(|p| p.auth()),
            Some(AuthKind::Account | AuthKind::None)
        )
    });
    match other {
        Some(other) => format!("/model {other} to pick one there, or upgrade at {checkout}"),
        None => format!("Upgrade at {checkout}, or set up a gateway with /connect."),
    }
}

/// What is said when a hosted model is asked for and the account has no
/// plan or credit for it: where to upgrade, and where else to pick one; and
/// when `used_up`, that its free allowance comes back at 00:00 UTC, or credit
/// pays for it till then. `pages` is where a subscription and credit are
/// bought.
fn unpaid_lines(elsewhere: Option<&str>, used_up: bool, pages: &[String; 2]) -> [String; 2] {
    let [checkout, credit] = pages;
    [
        match used_up {
            true => format!(
                "Left as it was: its free allowance is used up today and comes back at 00:00 UTC. Upgrade at {checkout}, or add credit at {credit}"
            ),
            false => format!(
                "Left as it was: Letronna's hosted models need a subscription. Upgrade at {checkout}"
            ),
        },
        match elsewhere {
            Some(gateway) => {
                format!("Or pick a model on {gateway}, which is ready: /model {gateway}")
            }
            None => "Or set up a gateway of your own, or a key, with /connect.".to_owned(),
        },
    ]
}

/// The model `filter` names exactly among `models`, as (gateway, model): on
/// `gateway`, the one in use, when it has it, or on the one gateway that
/// does. Else each gateway's that does, for a choice -- none when no name
/// is exact -- as the same id on two gateways is two different routes.
fn exactly(
    models: &[(String, String)],
    filter: &str,
    gateway: &str,
) -> Result<(String, String), Vec<(String, String)>> {
    let exact: Vec<(String, String)> = models
        .iter()
        .filter(|(_, m)| m.eq_ignore_ascii_case(filter))
        .cloned()
        .collect();
    if let Some(ours) = exact.iter().find(|(g, _)| g == gateway) {
        return Ok(ours.clone());
    }
    match exact.as_slice() {
        [only] => Ok(only.clone()),
        _ => Err(exact),
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

/// Switch searching with DuckDuckGo while no search service is chosen on or
/// off, and say what that means.
fn keyless_search(runner: &mut Runner, on: bool) {
    runner.store.settings.keyless_search = on;
    runner.persist();
    render::blank();
    render::line(&render::dim(if on {
        "Search with DuckDuckGo when no search key is set: on. With no service chosen, it is asked when the gateway's search cannot answer, or first where the gateway sells none. It needs no key, each search is asked first, and a service with a key (/search services) gives steadier results."
    } else {
        "Search with DuckDuckGo when no search key is set: off. Without a search service of your own (/search services), the model searches only through the gateway, where it sells search."
    }));
}

/// Which of Letronna's own searches a search through the account asks for:
/// `auto`, its free search first and the paid one when that finds nothing;
/// `free` alone, which uses no credit; or `paid` alone. With no word, says
/// which is used.
fn letronna_search(runner: &mut Runner, word: &str) {
    let now = store::search_named(&runner.store.settings.letronna_search);
    let next = match word {
        "" => now,
        "auto" | "free" | "paid" => word,
        _ => {
            render::blank();
            render::line(&render::red("Say /search letronna auto, free or paid."));
            return;
        }
    };
    if next != now {
        runner.store.settings.letronna_search = next.to_owned();
        runner.persist();
    }
    render::blank();
    render::line(&render::dim(match next {
        "free" => {
            "Letronna's search: free alone. It uses no credit, and can find less, or nothing while it is busy; DuckDuckGo answers then."
        }
        "paid" => "Letronna's search: paid alone, by the search from your credit, the steadiest.",
        _ => {
            "Letronna's search: auto. Its free search first, and the paid one, from your credit, when that finds nothing."
        }
    }));
}

/// Every search service and where it stands, then set one up to search with,
/// remove one, use DuckDuckGo alone, or switch it on or off. The one set up
/// last is used, and while none is, the gateway's search where it sells one,
/// with DuckDuckGo behind it.
pub fn search_services(runner: &mut Runner) {
    use letronna_core::search::{self, SERVICES};
    let keys = store::load_keys();
    let mut choice = search::choice();
    let account = letronna_core::auth::account();
    // With no service chosen, DuckDuckGo then answers only when the
    // account's search cannot.
    let behind_account = account.is_some() && choice.service.is_empty();
    let mut rows = vec![account_search_line(
        account.as_ref(),
        !choice.service.is_empty(),
    )];
    let mut choices: Vec<(String, &'static str, &'static str)> = Vec::new();
    for s in SERVICES {
        let (status, good, set_up) = search::status(s.name, &choice, &keys);
        let status = match (s.keyless && behind_account, status) {
            (true, "In use") => "If Letronna cannot",
            (_, status) => status,
        };
        // The keyless one has nothing to set up, so its row says what it is.
        let blurb = match s.keyless {
            true => format!(" · {}", s.blurb),
            false => String::new(),
        };
        rows.push(format!(
            "{} {:<10} {status}{blurb}",
            if good { "●" } else { "○" },
            s.name
        ));
        if s.keyless {
            // Chosen by name, it is the only one asked, so searches never go
            // through the gateway's paid search; with none chosen it is asked
            // after the gateway's, and its setting says whether at all.
            let alone = choice.service == s.name;
            if !alone {
                choices.push((format!("Use {} alone", s.name), s.name, "use"));
            }
            if alone || choice.service.is_empty() {
                let what = match good {
                    true => ("Switch off", "off"),
                    false => ("Switch on", "on"),
                };
                choices.push((format!("{} {}", what.0, s.name), s.name, what.1));
            }
            if !choice.service.is_empty() {
                let none = match account.is_some() {
                    true => "Use Letronna's search, then DuckDuckGo",
                    false => "Use the gateway's search, then DuckDuckGo",
                };
                choices.push((none.to_owned(), s.name, "none"));
            }
            continue;
        }
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
    if what == "on" || what == "off" {
        keyless_search(runner, what == "on");
        return;
    }
    if what == "use" || what == "none" {
        // The other services' keys and addresses are kept.
        choice.service = match what {
            "use" => name.to_owned(),
            _ => String::new(),
        };
        if let Err(error) = search::choose(&choice) {
            render::blank();
            render::line(&render::red(&error));
            return;
        }
        if what == "none" {
            render::blank();
            let signed_in = account.is_some();
            render::line(&render::dim(
                if signed_in && runner.store.settings.keyless_search {
                    "No search service is chosen: the model searches through your Letronna account, paid by the search from its credit, and with DuckDuckGo when that cannot answer. Each search is asked first."
                } else if signed_in {
                    "No search service is chosen: the model searches only through your Letronna account, paid by the search from its credit, as DuckDuckGo is switched off (/search keyless)."
                } else if runner.store.settings.keyless_search {
                    "No search service is chosen: the model searches through the gateway where it sells search, paid by the search, and with DuckDuckGo when that cannot answer. Each search is asked first."
                } else {
                    "No search service is chosen: the model searches only through the gateway, where it sells search, paid by the search, as DuckDuckGo is switched off (/search keyless)."
                },
            ));
            return;
        }
        runner.store.settings.keyless_search = true;
        runner.persist();
        render::blank();
        render::line(&render::dim(
            "The model now searches with DuckDuckGo alone, never through the gateway's paid search. It needs no key, each search is asked first, and a service with a key gives steadier results.",
        ));
        return;
    }
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

/// The Letronna row of `/search services`: where searching through the
/// account stands, `chosen` saying a service is used instead.
fn account_search_line(account: Option<&letronna_core::auth::Account>, chosen: bool) -> String {
    let name = letronna_core::BRAND;
    let Some(account) = account else {
        return format!(
            "○ {name:<10} Signed out · /login to search through your account, paid by the search"
        );
    };
    // An account never checked with the API has no plan yet, and its
    // credit is not known.
    let credit = account
        .team
        .as_ref()
        .map_or(account.credits, |team| team.credits);
    let broke = !account.plan.is_empty() && credit <= 0;
    match (chosen, broke) {
        (true, _) => format!("○ {name:<10} Not in use · the service you chose is used instead"),
        (false, true) => format!(
            "○ {name:<10} No credit · add some at letronna.ai to search through your account; DuckDuckGo answers until then"
        ),
        (false, false) => format!(
            "● {name:<10} In use · through your account, {} (/search letronna)",
            match store::letronna_search() {
                "free" => "its free search alone",
                "paid" => "paid by the search from its credit",
                _ => "free search first, then paid from its credit",
            }
        ),
    }
}

/// Every provider and where it stands, with its plan and usage, then
/// connect one, change it or disconnect it, or add a gateway of your own.
/// Everything connected is used at once; `/model` picks among all their
/// models.
pub fn connect(runner: &mut Runner) {
    use letronna_core::gateway::{self, LETRONNA, gateways};
    use letronna_core::providers::{self, AuthKind, router};
    let keys = store::load_keys();
    runner.store.follow_custom();
    let custom = runner.store.settings.custom_endpoint.clone();
    let (k, c) = (keys.clone(), custom.clone());
    let (models, _) = runner
        .bar
        .busy("asking every provider what it has", move || {
            gateway::all_models(&k, &c)
        });

    // The list sits in the box with its choices, not in the conversation.
    let mut rows = Vec::new();
    let mut choices: Vec<(String, &'static str, &'static str)> = Vec::new();
    let listed: Vec<_> = (gateways().into_iter())
        .filter(|g| providers::offered(g.name))
        .collect();
    // Wide enough for "ChatGPT · Work", where a provider has more accounts.
    let wide = (listed.iter().map(|g| g.name.chars().count()))
        .max()
        .unwrap_or(0)
        .max(11);
    // One a plugin offers is listed only while the plugin is on.
    for g in &listed {
        let further = providers::accounts::get(g.name).is_some();
        let count = models.iter().filter(|(n, _)| n == g.name).count();
        let (status, good, set_up) = gateway::status(g.name, &keys, &custom, count);
        let usage = router::usage(g.name);
        let status = if usage.is_empty() || !set_up {
            status
        } else {
            format!("{status} · {usage}")
        };
        let own = gateway::is_own(g.name);
        // Your own say where they are, as their names are yours.
        let status = match own {
            true => format!("{status} · {}", gateway::base_url(g.name, &custom)),
            false => status,
        };
        rows.push(format!(
            "{} {:<wide$} {status}",
            if good { "●" } else { "○" },
            g.name
        ));
        let provider = providers::find(g.name);
        if !set_up
            && !further
            && let Some(caveat) = provider.and_then(|p| p.caveat())
        {
            rows.push(format!("  {:<wide$} {}", "", g.blurb));
            rows.push(format!("  {:<wide$} {caveat}", ""));
        }
        let auth = provider.map(|p| p.auth());
        let signs_in = match auth {
            Some(AuthKind::None) => continue,
            Some(AuthKind::Account | AuthKind::OAuth(_)) => true,
            _ => false,
        };
        // A sign-in that can be finished on a phone, or over SSH.
        let away = matches!(auth, Some(AuthKind::OAuth(spec))
            if spec.device.is_some() || spec.manual_redirect.is_some());
        match (signs_in, set_up) {
            (true, false) => {
                choices.push((format!("Sign in to {}", g.name), g.name, "in"));
                if away {
                    choices.push((
                        format!("Sign in to {} with a code, on another device", g.name),
                        g.name,
                        "away",
                    ));
                }
            }
            (true, true) => choices.push((format!("Sign out of {}", g.name), g.name, "out")),
            (false, false) => choices.push((format!("Set up {}", g.name), g.name, "set")),
            (false, true) => {
                choices.push((format!("Change {}", g.name), g.name, "set"));
                if !further {
                    choices.push((format!("Remove {}", g.name), g.name, "remove"));
                }
            }
        }
        if further {
            if providers::accounts::of(providers::accounts::provider_of(g.name).unwrap_or(""))
                .first()
                != Some(&g.name)
            {
                choices.push((format!("Use {} first", g.name), g.name, "first"));
            }
            choices.push((format!("Remove the account {}", g.name), g.name, "drop"));
        } else if set_up && providers::accounts::takes_more(g.name) {
            choices.push((format!("Add another {} account", g.name), g.name, "more"));
        }
    }
    choices.push((
        "Add a gateway of your own (LM Studio, vLLM, a proxy…)".to_owned(),
        "",
        "add",
    ));
    let mut options: Vec<&str> = choices.iter().map(|c| c.0.as_str()).collect();
    options.push("Done");
    let Some(picked) = runner.bar.choose("Accounts", rows, &options) else {
        return;
    };
    let Some((_, name, what)) = choices.get(picked).cloned() else {
        return;
    };
    match what {
        "in" if name == LETRONNA => sign_in(runner),
        "out" if name == LETRONNA => sign_out(runner),
        "in" => provider_sign_in(runner, name),
        "out" => {
            render::blank();
            match providers::auth::sign_out(name) {
                Ok(()) => render::line(&render::dim(&format!(
                    "Signed out of {name}. Its models leave /model."
                ))),
                Err(error) => render::line(&render::red(&error)),
            }
        }
        "remove" => {
            let removed = match gateway::is_own(name) {
                true => gateway::remove_own(name),
                false => store::save_key(gateway::key_slot(name), None)
                    .map(|_| ())
                    .map_err(|e| e.to_string()),
            };
            render::blank();
            if let Err(error) = removed {
                render::line(&render::red(&format!("Could not remove {name}: {error}")));
                return;
            }
            if name == "Custom" {
                runner.store.settings.custom_endpoint.clear();
            }
            render::line(&render::dim(&format!(
                "{name} removed. Its models leave /model."
            )));
        }
        "add" => add_own_gateway(runner),
        "away" => provider_sign_in_away(runner, name),
        "more" => add_account(runner, name),
        "first" => {
            render::blank();
            let moved = (0..providers::accounts::of(name).len())
                .try_for_each(|_| providers::accounts::shift(name, true));
            match moved {
                Ok(()) => render::line(&render::dim(&format!("{name} is reached for first now."))),
                Err(error) => render::line(&render::red(&error)),
            }
        }
        "drop" => {
            render::blank();
            match providers::accounts::remove(name) {
                Ok(()) => render::line(&render::dim(&format!(
                    "{name} removed, with its sign-in or key. Its models leave /model."
                ))),
                Err(error) => render::line(&render::red(&error)),
            }
        }
        _ => set_up_gateway(runner, name),
    }
    runner.persist();
}

/// Add another account of `provider`: a name for it, then its sign-in or
/// key, then a check that it answers.
fn add_account(runner: &mut Runner, provider: &'static str) {
    use letronna_core::providers::{self, AuthKind, accounts};
    render::blank();
    if let Some(caveat) = providers::find(provider).and_then(|p| p.caveat()) {
        render::line(&render::dim(caveat));
    }
    if matches!(
        providers::find(provider).map(|p| p.info().tier),
        Some(providers::Tier::Subscription)
    ) {
        render::line(&render::dim(&format!(
            "Using another account to get past a plan's limits may be against {provider}'s terms."
        )));
    }
    let suggested = accounts::next_label(provider);
    let label = runner
        .bar
        .ask(
            &format!("A name for this {provider} account, like Work (Enter for {suggested})"),
            false,
        )
        .unwrap_or_default();
    let label = match label.trim() {
        "" => suggested,
        given => given.to_owned(),
    };
    let name = match accounts::add(provider, &label) {
        Ok(name) => name,
        Err(error) => return render::line(&render::red(&error)),
    };
    match providers::find(name).map(|p| p.auth()) {
        Some(AuthKind::OAuth(_)) => {
            let options = ["In the browser here", "With a code, on another device"];
            let picked = runner
                .bar
                .choose(&format!("Sign in to {name}"), Vec::new(), &options);
            match picked {
                Some(1) => provider_sign_in_away(runner, name),
                Some(_) => provider_sign_in(runner, name),
                // Not signed in yet: it waits in /connect for that.
                None => {}
            }
        }
        _ => {
            if ask_key(runner, name) {
                runner.persist();
                check_gateway(runner, name);
            }
        }
    }
}

/// Sign in to a provider on another device: a code to type there, or a page
/// that shows one to paste back here.
fn provider_sign_in_away(runner: &mut Runner, name: &'static str) {
    use letronna_core::providers::auth;
    use std::sync::atomic::{AtomicBool, Ordering};
    render::blank();
    let flow = match auth::begin_away(name) {
        Ok(flow) => flow,
        Err(error) => return render::line(&render::red(&error)),
    };
    render::said(
        &render::blue("●"),
        &render::bold(&format!("Sign in to {name}")),
    );
    match &flow.code {
        Some(code) => {
            render::line(&render::dim("On any device, open:"));
            render::line(&flow.url);
            render::line(&format!(
                "{} {}",
                render::dim("and type the code"),
                render::bold(code)
            ));
        }
        None => {
            render::line(&render::dim(
                "On any device, open this and sign in; the page then shows a code:",
            ));
            render::line(&flow.url);
            let pasted = runner
                .bar
                .ask("Paste the code the page shows", false)
                .unwrap_or_default();
            if pasted.trim().is_empty() {
                flow.cancel();
                return render::line(&render::dim("Sign-in cancelled."));
            }
            if let Err(error) = flow.paste(pasted.trim()) {
                flow.cancel();
                return render::line(&render::red(&error));
            }
        }
    }
    let stop = std::sync::Arc::new(AtomicBool::new(false));
    let watch = (stop.clone(), flow.clone());
    let waiting = {
        let flow = flow.clone();
        std::thread::spawn(move || flow.finish())
    };
    let done = runner.bar.busy_cancellable(
        "waiting for the sign-in · esc to cancel",
        stop,
        move || {
            while !waiting.is_finished() {
                if watch.0.load(Ordering::Relaxed) {
                    watch.1.cancel();
                }
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
            waiting
                .join()
                .unwrap_or_else(|_| Err("The sign-in stopped.".into()))
        },
    );
    match done {
        Ok(_) => {
            render::line(&render::dim(&format!("Signed in to {name}.")));
            check_gateway(runner, name);
        }
        Err(error) => render::line(&render::red(&error)),
    }
}

/// Each account's plan and how much of each of its windows is left, asked
/// of the plans that have a page for it.
fn limits(runner: &mut Runner) {
    use letronna_core::gateway;
    use letronna_core::providers::{self, usage};
    let keys = store::load_keys();
    let custom = runner.store.settings.custom_endpoint.clone();
    let ready: Vec<&'static str> = (gateway::ready(&keys, &custom).into_iter())
        .map(|(name, ..)| name)
        .filter(|name| *name != gateway::LETRONNA)
        .filter(|name| providers::find(name).is_some())
        .collect();
    let asked = ready.clone();
    let known = runner.bar.busy("asking each plan what is left", move || {
        (asked.iter())
            .map(|name| (*name, usage::check(name, true)))
            .collect::<Vec<_>>()
    });
    render::blank();
    let now = letronna_core::chat::now();
    let mut any = false;
    for (name, known) in known {
        let usage = match known {
            Ok(usage) if usage.windows.is_empty() && usage.note.is_none() => continue,
            Ok(usage) => usage,
            Err(error) => {
                render::line(&format!("{} {}", render::bold(name), render::red(&error)));
                any = true;
                continue;
            }
        };
        any = true;
        let plan = (usage.plan.as_deref())
            .map(|p| format!(" · {}", usage::capitalised(p)))
            .unwrap_or_default();
        render::line(&format!("{}{}", render::bold(name), render::dim(&plan)));
        if let Some(note) = &usage.note {
            render::line(&render::dim(&format!("  {note}")));
        }
        for window in &usage.windows {
            let line = format!("  {}", usage::window_line(window, now));
            render::line(&match window.full(now) {
                true => render::red(&line),
                false => line,
            });
        }
    }
    if !any {
        render::line(&render::dim(
            "No plan has said yet. /connect signs in to ChatGPT or Claude, or takes an OpenCode Go/Zen key.",
        ));
    }
}

/// Ask for a gateway's address where it takes one and its key, save them,
/// and ask it for its models, so the user knows at once whether it works.
fn set_up_gateway(runner: &mut Runner, name: &'static str) {
    use letronna_core::gateway;
    let own = gateway::is_own(name);
    if own || name == "Custom" {
        let now = gateway::base_url(name, &runner.store.settings.custom_endpoint);
        let hint = match now.is_empty() {
            true => format!("{name} base URL, like http://localhost:8000/v1"),
            false => format!("{name} base URL (Enter keeps {now})"),
        };
        let given = runner.bar.ask(&hint, false).unwrap_or_default();
        if !given.trim().is_empty() {
            let kept = match own {
                true => gateway::change_own(name, &given),
                false => gateway::own_base(&given),
            };
            match kept {
                Ok(base) if !own => runner.store.settings.custom_endpoint = base,
                Ok(_) => {}
                Err(error) => {
                    render::blank();
                    render::line(&render::red(&error));
                    return;
                }
            }
        } else if now.is_empty() {
            return;
        }
    }
    if !ask_key(runner, name) {
        return;
    }
    runner.persist();
    check_gateway(runner, name);
}

/// Add a gateway of the user's own: its name, address and key, then a check.
fn add_own_gateway(runner: &mut Runner) {
    use letronna_core::gateway;
    let name = runner
        .bar
        .ask("A name for it, like LM Studio or Office vLLM", false)
        .unwrap_or_default();
    if name.trim().is_empty() {
        return;
    }
    // A name a gateway has already: change that one, or add this one under
    // the next free name -- never one in place of the other unasked.
    let name = match gateway::free_name(&name) {
        None => name.trim().to_owned(),
        Some(free) => {
            let holder = gateway::holder(&name);
            let spelled = holder.unwrap_or(name.trim());
            // Numbered as the one there is spelled.
            let free = gateway::free_name(spelled).unwrap_or(free);
            let mut options = Vec::new();
            let change = holder
                .filter(|holder| gateway::is_own(holder))
                .map(|holder| format!("Change {holder}'s address and key"));
            options.extend(change.as_deref());
            let instead = format!("Add this one as {free}");
            options.extend([instead.as_str(), "Cancel"]);
            let question = format!("There is a gateway called {spelled} already");
            let Some(picked) = runner.bar.choose(&question, Vec::new(), &options) else {
                return;
            };
            match (options[picked], holder) {
                (option, Some(holder)) if change.as_deref() == Some(option) => {
                    set_up_gateway(runner, holder);
                    return;
                }
                (option, _) if option == instead => free,
                _ => return,
            }
        }
    };
    let address = runner
        .bar
        .ask(
            &format!("{name} base URL, like http://localhost:1234/v1"),
            false,
        )
        .unwrap_or_default();
    if address.trim().is_empty() {
        return;
    }
    let name = match gateway::add_own(&name, &address) {
        Ok((name, _)) => name,
        Err(error) => {
            render::blank();
            render::line(&render::red(&error));
            return;
        }
    };
    if ask_key(runner, name) {
        check_gateway(runner, name);
    }
}

/// Ask for `name`'s key and save it: Enter keeps the one saved, or none
/// for a gateway that needs none. Whether the key is as wanted.
fn ask_key(runner: &mut Runner, name: &str) -> bool {
    use letronna_core::gateway;
    let saved = store::load_keys()
        .get(gateway::key_slot(name))
        .is_some_and(|key| !key.trim().is_empty());
    let hint = match (gateway::gateway(name).needs_key, saved) {
        (_, true) => format!("{name} API key (Enter keeps the one saved)"),
        (true, false) => format!("{name} API key"),
        (false, false) => format!("{name} API key, if it takes one (Enter for none)"),
    };
    let key = runner.bar.ask(&hint, true).unwrap_or_default();
    if key.trim().is_empty() {
        return true;
    }
    if let Err(error) = store::save_key(gateway::key_slot(name), Some(key.trim())) {
        render::blank();
        render::line(&render::red(&format!("Could not save the key: {error}")));
        return false;
    }
    true
}

/// Ask `name` for its models and say how that went.
fn check_gateway(runner: &mut Runner, name: &str) {
    let keys = store::load_keys();
    let custom = runner.store.settings.custom_endpoint.clone();
    let asked = name.to_owned();
    let models = runner
        .bar
        .busy(&format!("asking {name} for its models"), move || {
            letronna_core::gateway::models_of(&asked, &keys, &custom)
        });
    render::blank();
    match models {
        Ok(models) if !models.is_empty() => render::line(&format!(
            "{} {} {}",
            render::green("●"),
            render::bold(name),
            render::dim(&format!(
                "· {} model{} · /model lists them",
                models.len(),
                if models.len() == 1 { "" } else { "s" }
            ))
        )),
        Ok(_) => render::line(&render::amber(&format!(
            "{name} is saved, but it offered no models. Check its address and key with /connect."
        ))),
        Err(error) => {
            render::line(&render::amber(&format!(
                "{name} is saved, but it did not answer: {error}"
            )));
            render::line(&render::dim(
                "Change its address or key with /connect, or remove it there.",
            ));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        Access, Asked, Back, Flow, Run, Stage, ToolSearch, account_search_line, agent_lines,
        agent_rows, asked, choice_label, command, crew_switch, exactly, from_match, held_line,
        keep_days, last_prompt, leaving, model_label, name_label, names_setting, naming_said,
        offered, point_label, ranking, rewind_points, rewinds, run_rows, side_setting, standing,
        tool_search_asked, turn_files, turn_starts, turns_from, typed_path, unpaid_instead,
        unpaid_lines,
    };
    use crate::session::Runner;
    use letronna_core::agents::Seen;
    use letronna_core::chat::Message;
    use letronna_core::driver::Status;
    use letronna_core::gate::Mode;
    use letronna_core::memory::{Saving, Scope};
    use letronna_core::store;
    use letronna_core::workflow::{AgentState, AgentView, PhaseView, RunState, RunView};
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
    fn model_names_takes_auto_chat_off_or_a_model() {
        assert_eq!(names_setting(""), None);
        assert_eq!(names_setting("auto"), Some((true, Some(String::new()))));
        assert_eq!(names_setting("chat"), Some((true, Some("chat".into()))));
        assert_eq!(names_setting(" off "), Some((false, None)));
        assert_eq!(names_setting("on"), Some((true, None)));
        assert_eq!(
            names_setting("small-model"),
            Some((true, Some("small-model".into())))
        );
        assert!(naming_said(false, "chat").starts_with("Sessions keep the first line"));
        let auto = naming_said(true, "");
        assert!(auto.contains("Letronna's quick model for a chat on Letronna"));
        assert!(auto.contains("A chat on another gateway is named there."));
        assert!(naming_said(true, "small-model").starts_with("small-model names each session"));
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
    fn rewind_refuses_a_flag_it_does_not_know() {
        assert!(rewinds(""));
        // What /undo once needed for the files, now a choice of its own.
        assert!(rewinds("--files"));
        assert!(!rewinds("--file"));
        assert!(!rewinds("3"));
    }

    /// A tool's result that changed `paths`.
    fn wrote(paths: &[&str]) -> Message {
        Message {
            files: (paths.iter())
                .map(|path| letronna_core::history::FileChange {
                    path: path.into(),
                    before: None,
                    after: "f00d".into(),
                    mode: None,
                    made: 0,
                })
                .collect(),
            ..said("tool", "done")
        }
    }

    #[test]
    fn rewind_points_are_the_turns_you_started_newest_first() {
        let messages = [
            said("user", "first"),
            said("assistant", "ok"),
            said("user", "second"),
            // Typed while the reply ran, so part of the second turn.
            Message {
                folded: true,
                ..said("user", "and the tests")
            },
            said("assistant", "both done"),
            // Letronna's, not yours.
            Message {
                notice: true,
                ..said("user", "[Letronna notice]\n- job #7 exited with code 0")
            },
            said("assistant", "they passed"),
            said("user", "third"),
        ];
        assert_eq!(rewind_points(&messages), [7, 2, 0]);
        assert!(rewind_points(&[said("assistant", "hello")]).is_empty());
    }

    #[test]
    fn a_point_counts_the_files_its_turn_changed() {
        let messages = [
            said("user", "fix the parser"),
            said("assistant", ""),
            wrote(&["/p/src/parse.rs"]),
            said("assistant", ""),
            wrote(&["/p/src/parse.rs", "/p/tests/parse.rs"]),
            said("assistant", "fixed"),
            said("user", "thanks"),
            said("assistant", "any time"),
        ];
        assert_eq!(turn_files(&messages, 0), 2);
        assert_eq!(turn_files(&messages, 6), 0);
        assert_eq!(point_label(&messages, 0, 40), "fix the parser  ·  2 files");
        assert_eq!(point_label(&messages, 6, 40), "thanks");
        // Cut to fit, the count stays.
        assert_eq!(point_label(&messages, 0, 20), "fix the…  ·  2 files");
        // Words from a pasted escape are shown, the escape is not.
        let pasted = [said("user", "look at \x1b[31mthis")];
        assert_eq!(point_label(&pasted, 0, 40), "look at [31mthis");
    }

    #[test]
    fn going_back_offers_the_code_only_where_a_file_can_go_back() {
        let labels = |code, summarizes| -> Vec<&str> {
            offered(code, summarizes)
                .into_iter()
                .map(Back::label)
                .collect()
        };
        assert_eq!(
            labels(true, true),
            [
                "Restore code and conversation",
                "Restore conversation",
                "Restore code",
                "Summarize up to here",
                "Branch from here",
                "Never mind"
            ]
        );
        assert_eq!(
            labels(false, false),
            ["Restore conversation", "Branch from here", "Never mind"]
        );
        assert!(leaving(1).starts_with("Restoring the conversation removes this message from"));
        assert!(leaving(2).contains("and the 1 message after it"));
        assert!(leaving(7).contains("and the 6 messages after it"));
    }

    #[test]
    fn summarizing_up_to_a_point_keeps_the_turns_from_it() {
        let messages = [
            said("user", "first"),
            said("assistant", "ok"),
            said("user", "second"),
            said("assistant", "ok"),
            said("user", "third"),
            said("assistant", "ok"),
        ];
        // /compact keeps that many last turns, so its summary stops at the
        // message picked.
        let keep = turns_from(&messages, 2);
        assert_eq!(keep, 2);
        assert_eq!(turn_starts(&messages).nth_back(keep - 1), Some(2));
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
            idle: false,
            started,
            status: Arc::new(status),
        }
    }

    #[test]
    fn a_kept_helper_waiting_for_work_is_listed_as_idle() {
        let started = Instant::now();
        let mut kept = helper("keeper", Some("finished"), "Keep watch", started);
        kept.idle = true;
        let rows = agent_rows(&[kept], started, 100);
        assert!(rows[1].contains("idle, waiting for more work"), "{rows:?}");
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

    #[test]
    fn retention_takes_days_or_off_and_refuses_anything_else() {
        assert_eq!(keep_days(""), Ok(None));
        assert_eq!(keep_days(" 30 "), Ok(Some(30)));
        assert_eq!(keep_days("off"), Ok(Some(0)));
        assert_eq!(keep_days("0"), Ok(Some(0)));
        assert!(keep_days("a month").is_err());
        assert!(keep_days("-3").is_err());
    }

    #[test]
    fn a_model_named_exactly_stays_on_the_gateway_in_use() {
        let on = |g: &str, m: &str| (g.to_owned(), m.to_owned());
        let models = [
            on("Custom", "gpt-x"),
            on("Letronna", "gpt-x"),
            on("Letronna", "only-here"),
        ];
        // Before, the first gateway by name took it, whichever was in use.
        assert_eq!(
            exactly(&models, "GPT-X", "Letronna"),
            Ok(on("Letronna", "gpt-x"))
        );
        assert_eq!(
            exactly(&models, "only-here", "Custom"),
            Ok(on("Letronna", "only-here"))
        );
        // On two gateways, neither in use: a choice of the two.
        assert_eq!(exactly(&models, "gpt-x", "OpenAI").unwrap_err().len(), 2);
        assert_eq!(exactly(&models, "gpt", "Letronna"), Err(Vec::new()));
    }

    #[test]
    fn a_hosted_model_that_needs_a_subscription_is_marked_and_refused_kindly() {
        assert_eq!(
            model_label("gpt-x", "Letronna", false, Access::Unpaid),
            "gpt-x · Letronna (needs a subscription)"
        );
        assert_eq!(
            model_label("gpt-x", "Letronna", true, Access::Unpaid),
            "gpt-x · Letronna (in use · needs a subscription)"
        );
        assert_eq!(
            model_label("m", "Custom", true, Access::Open),
            "m · Custom (in use)"
        );
        assert_eq!(
            model_label("m", "Custom", false, Access::Open),
            "m · Custom"
        );
        assert_eq!(
            model_label("fast", "Letronna", false, Access::Free),
            "fast · Letronna (free today)"
        );
        assert_eq!(
            model_label("smart", "Letronna", true, Access::UsedUp),
            "smart · Letronna (in use · free · used up today)"
        );

        let checkout = "https://letronna.ai/dashboard/checkout";
        let pages = [checkout.to_owned(), format!("{checkout}?plan=credits")];
        let [why, instead] = unpaid_lines(Some("Custom"), false, &pages);
        assert_eq!(
            why,
            "Left as it was: Letronna's hosted models need a subscription. Upgrade at https://letronna.ai/dashboard/checkout"
        );
        assert_eq!(
            instead,
            "Or pick a model on Custom, which is ready: /model Custom"
        );
        let [_, nowhere] = unpaid_lines(None, false, &pages);
        assert!(nowhere.contains("/connect"));
        // Free on a perk whose allowance is used up: back at midnight UTC,
        // or paid for till then.
        let [why, instead] = unpaid_lines(Some("Custom"), true, &pages);
        assert_eq!(
            why,
            "Left as it was: its free allowance is used up today and comes back at 00:00 UTC. Upgrade at https://letronna.ai/dashboard/checkout, or add credit at https://letronna.ai/dashboard/checkout?plan=credits"
        );
        assert_eq!(
            instead,
            "Or pick a model on Custom, which is ready: /model Custom"
        );
        assert_eq!(
            held_line(true, "Upgrade at x."),
            "Not sent: this model's free allowance is used up today and comes back at 00:00 UTC. Upgrade at x."
        );
        assert_eq!(
            held_line(false, "Upgrade at x."),
            "Not sent: Letronna's hosted models need a subscription. Upgrade at x."
        );

        // Ollama and the hosted pool are not where a message is sent instead.
        assert_eq!(
            unpaid_instead(&["Letronna", "Ollama", "OpenAI"], checkout),
            "/model OpenAI to pick one there, or upgrade at https://letronna.ai/dashboard/checkout"
        );
        let alone = unpaid_instead(&["Letronna", "Ollama"], checkout);
        assert!(alone.contains("/connect") && !alone.contains("/model Ollama"));
    }

    #[test]
    fn the_letronna_search_row_says_where_the_account_stands() {
        use letronna_core::auth::Account;
        let checked = |credits| Account {
            plan: "free".into(),
            credits,
            ..Account::default()
        };
        assert!(account_search_line(None, false).contains("Signed out · /login"));
        assert!(account_search_line(Some(&checked(500)), false).starts_with("● Letronna   In use"));
        assert!(account_search_line(Some(&checked(-1)), false).contains("No credit"));
        assert!(account_search_line(Some(&checked(500)), true).contains("Not in use"));
        // Not checked with the API yet, its credit is not known to be gone.
        let unchecked = Account::default();
        assert!(account_search_line(Some(&unchecked), false).contains("In use"));
    }

    #[test]
    fn tool_search_takes_a_ranking_a_key_an_address_or_a_model() {
        assert_eq!(tool_search_asked(""), Ok(ToolSearch::Show));
        assert_eq!(tool_search_asked(" check"), Ok(ToolSearch::Check));
        assert_eq!(ranking("names").1, "Words only");
        assert_eq!(ranking("").1, "Automatic");
        assert_eq!(tool_search_asked(" jev "), Ok(ToolSearch::Rank("jev")));
        assert_eq!(tool_search_asked("names"), Ok(ToolSearch::Rank("names")));
        assert_eq!(
            tool_search_asked("key remove"),
            Ok(ToolSearch::Key { remove: true })
        );
        assert_eq!(
            tool_search_asked("address https://jev.example/v1/systemone"),
            Ok(ToolSearch::Address(
                "https://jev.example/v1/systemone".into()
            ))
        );
        assert_eq!(
            tool_search_asked("address letronna"),
            Ok(ToolSearch::Address(String::new()))
        );
        assert_eq!(
            tool_search_asked("model jev-1.13"),
            Ok(ToolSearch::Model("jev-1.13".into()))
        );
        // A key is never sent in the clear to another machine.
        assert!(tool_search_asked("address http://jev.example/x").is_err());
        assert!(tool_search_asked("address http://127.0.0.1:9/x").is_ok());
        assert!(tool_search_asked("sometimes").is_err());
    }

    #[test]
    fn side_model_takes_a_model_or_off_and_shows_it_bare() {
        assert_eq!(side_setting(""), None);
        assert_eq!(side_setting(" small-model "), Some("small-model".into()));
        assert_eq!(side_setting("off"), Some(String::new()));
    }

    #[test]
    fn workflow_words_read_as_a_verb_and_a_run() {
        assert_eq!(asked(""), Asked::List);
        assert_eq!(asked("3"), Asked::Show("wf-3".into()));
        assert_eq!(asked("show wf-2"), Asked::Show("wf-2".into()));
        assert_eq!(asked("stop 4"), Asked::Stop("wf-4".into()));
        assert_eq!(asked("resume  wf-1"), Asked::Resume("wf-1".into()));
        assert_eq!(asked("stop"), Asked::Which("stop"));
        assert_eq!(asked("pause 1"), Asked::Pause("wf-1".into()));
        assert_eq!(asked("pause"), Asked::Which("pause"));
        assert_eq!(
            asked("skip wf-1 #2"),
            Asked::Skip("wf-1".into(), "#2".into())
        );
        // A label may take several words.
        assert_eq!(
            asked("retry 3 src/a.rs:12  vote 2"),
            Asked::Retry("wf-3".into(), "src/a.rs:12 vote 2".into())
        );
        assert_eq!(
            asked("retry wf-3"),
            Asked::WhichAgent("retry", "wf-3".into())
        );
        assert_eq!(asked("skip"), Asked::Which("skip"));
        // A typo is not taken for a run's name, which would seem to have worked.
        assert_eq!(asked("stpo wf-3"), Asked::Unclear);
        assert_eq!(asked("stop wf-1 wf-2"), Asked::Unclear);
    }

    #[test]
    fn a_runs_view_gives_its_state_and_its_agents_under_their_phases() {
        let agent = |number: usize, label: &str, phase: &str, state, tokens| AgentView {
            number,
            label: label.into(),
            phase: phase.into(),
            state,
            tokens,
        };
        let mut view = RunView {
            name: "wf-3".into(),
            title: "audit".into(),
            state: RunState::Paused,
            phases: vec![
                PhaseView {
                    title: "Find".into(),
                    agents: vec![agent(1, "finder", "Find", AgentState::Done, 12_300)],
                },
                PhaseView {
                    title: "Check".into(),
                    agents: vec![
                        agent(2, "src/a.rs", "Check", AgentState::Replayed, 0),
                        agent(3, "wf-3 #3", "Check", AgentState::Running, 900),
                    ],
                },
                PhaseView {
                    title: "Report".into(),
                    agents: Vec::new(),
                },
            ],
            spent: 13_200,
            budget: 3_000_000,
            seconds: 62,
        };
        assert_eq!(standing(&view), "paused · 1m02s · 2 of 3 done");
        assert_eq!(
            agent_lines(&view),
            [
                "Find",
                "  #1   finder    done      12k",
                "Check",
                "  #2   src/a.rs  replayed  -",
                "  #3   wf-3 #3   running   900",
                "Report",
            ]
        );
        view.state = RunState::Ended("stopped".into());
        assert_eq!(standing(&view), "stopped · 1m02s");
    }

    #[test]
    fn run_rows_line_up_and_fit_the_width() {
        let run = |name: &str, state: &str, title: &str| Run {
            name: name.into(),
            title: title.into(),
            state: state.into(),
            tokens: "1.2M".into(),
            stage: Stage::Running,
        };
        let runs = [
            run("wf-3", "running · 4s · 1 of 2 done", "unwrap-audit"),
            run("wf-10", "stopped · 9s", "review"),
        ];
        let rows = run_rows(&runs, 80);
        assert_eq!(
            rows[0],
            "run    state                       tokens  workflow"
        );
        assert_eq!(
            rows[1],
            "wf-3   running · 4s · 1 of 2 done  1.2M    unwrap-audit"
        );
        assert_eq!(rows[2], "wf-10  stopped · 9s                1.2M    review");
        // The title goes first, so the run is still told apart.
        let narrow = run_rows(&runs, 40);
        assert_eq!(narrow[1], "wf-3   running · 4s · 1 of 2 done  1.2M…");
        assert!(narrow.iter().all(|row| row.chars().count() <= 40));
    }

    #[test]
    fn redo_puts_back_the_latest_undo_only_while_nothing_new_was_said() {
        let undo = |session: u64, left: usize, said: &str| super::Undone {
            session,
            left,
            messages: Some(Vec::new()),
            files: None,
            said: said.to_owned(),
        };
        fn said(taken: Result<super::Undone, &'static str>) -> Result<String, &'static str> {
            taken.map(|undone| undone.said)
        }
        assert!(
            said(super::redoable(7, 3))
                .unwrap_err()
                .starts_with("Nothing to redo")
        );
        // Two rewinds come back latest first, each where the one after it
        // left the session.
        super::keep_undone(undo(7, 5, "first"));
        super::keep_undone(undo(7, 2, "second"));
        assert_eq!(said(super::redoable(7, 2)), Ok("second".to_owned()));
        assert_eq!(said(super::redoable(7, 5)), Ok("first".to_owned()));
        assert!(super::redoable(7, 5).is_err());
        // Another session's rewind is not this one's to put back, and one
        // of its own starts the list over.
        super::keep_undone(undo(7, 4, "elsewhere"));
        assert!(super::redoable(8, 4).is_err());
        super::keep_undone(undo(8, 1, "here"));
        assert!(super::redoable(7, 4).is_err());
        // Something said since, and no rewind before it holds either.
        super::keep_undone(undo(8, 3, "later"));
        let refused = said(super::redoable(8, 4)).unwrap_err();
        assert!(refused.starts_with("Not redone"), "{refused}");
        assert!(super::redoable(8, 1).is_err());
    }

    #[test]
    fn crew_turns_over_or_goes_the_way_it_is_told() {
        assert_eq!(crew_switch("", false), Some(true));
        assert_eq!(crew_switch("", true), Some(false));
        assert_eq!(crew_switch("on", true), Some(true));
        assert_eq!(crew_switch("off", false), Some(false));
        assert_eq!(crew_switch("maybe", false), None);
    }

    #[test]
    fn slash_mode_auto_hands_what_would_ask_to_the_judge() {
        let mut runner = Runner {
            store: Default::default(),
            allowed: Default::default(),
            mode: Mode::Default,
            human: true,
            helpers_answered: true,
            trouble: None,
            bar: crate::bar::Bar::new(String::new()),
            stopped: false,
            images: Vec::new(),
            notices: letronna_core::notify::Notices::new(Box::new(crate::notify::Silent)),
            ended: None,
            voice: None,
            review: Default::default(),
            plain_terminal: false,
            way_out: None,
        };
        let flow = command(&mut runner, "/mode auto", &mut 0, None);
        assert!(matches!(flow, Flow::Handled));
        assert_eq!(runner.mode, Mode::Auto);
        command(&mut runner, "/mode fast", &mut 0, None);
        assert_eq!(runner.mode, Mode::Auto);
    }

    #[test]
    fn slash_remember_with_words_asks_in_them_and_bare_off_a_keyboard_only_says() {
        let mut runner = Runner {
            store: Default::default(),
            allowed: Default::default(),
            mode: Mode::Default,
            human: true,
            helpers_answered: true,
            trouble: None,
            bar: crate::bar::Bar::new(String::new()),
            stopped: false,
            images: Vec::new(),
            notices: letronna_core::notify::Notices::new(Box::new(crate::notify::Silent)),
            ended: None,
            voice: None,
            review: Default::default(),
            plain_terminal: false,
            way_out: None,
        };
        runner.store.conversations.push(Default::default());
        let sent = |runner: &mut Runner, line: &str| match command(runner, line, &mut 0, None) {
            Flow::Send(text) => Some(text),
            _ => None,
        };
        assert_eq!(
            sent(&mut runner, "/remember I use pnpm").as_deref(),
            Some("Remember this: I use pnpm")
        );
        assert_eq!(
            sent(&mut runner, "/remember --project build with make").as_deref(),
            Some("Remember for this project: build with make")
        );
        assert_eq!(
            sent(&mut runner, "/remember --chat the flight is at 9").as_deref(),
            Some("Remember for this chat only: the flight is at 9")
        );
        // Nothing to keep after the flag, and nothing chosen with no keyboard.
        assert_eq!(sent(&mut runner, "/remember --chat"), None);
        assert_eq!(sent(&mut runner, "/remember"), None);
        assert_eq!(runner.store.conversations[0].memory_saving, None);
    }

    #[test]
    fn making_memory_the_default_sets_it_and_leaves_the_chat_following_it() {
        let mut store = store::Store::default();
        store.conversations.push(Default::default());
        store.choose_memory_saving(0, Some(Saving::Auto));
        store.choose_memory_scope(0, Some(Scope::Chat));

        super::make_memory_default(&mut store, 0);

        assert_eq!(store.settings.memory_saving, Saving::Auto);
        assert_eq!(store.settings.memory_scope, Scope::Chat);
        // No choice of its own left, so a later change of the default moves it.
        assert_eq!(store.conversations[0].memory_saving, None);
        assert_eq!(store.conversations[0].memory_scope, None);
        store.settings.memory_saving = Saving::Ask;
        assert_eq!(store.memory_saving(0), Saving::Ask);
    }

    #[test]
    fn a_picture_with_a_quote_in_its_name_is_attached_by_its_whole_path() {
        for name in ["say \"cheese\".png", "it's here.png", "a\" b.png"] {
            let path = std::path::Path::new("/Users/someone/shots").join(name);
            // Split from its command as `command` splits any line.
            let line = crate::picture::image_line(&path);
            let (_, rest) = line.split_once(char::is_whitespace).unwrap();
            assert_eq!(super::typed_path(rest.trim(), None), path);
        }
    }

    #[test]
    fn a_tool_family_reads_as_on_until_every_tool_in_it_is_off() {
        use letronna_core::toolset::Family;
        let files = Family::Files.tools();
        let row = super::family_row(Family::Files, &[], None);
        assert!(row.on);
        assert_eq!(row.about, Family::Files.blurb());
        let row = super::family_row(Family::Files, &files[..2], None);
        assert!(row.on);
        assert!(row.about.ends_with(&format!("2 of {} off", files.len())));
        let row = super::family_row(Family::Files, &files, Some("no search key"));
        assert!(!row.on);
        assert!(row.about.ends_with(" · no search key"));
    }

    #[test]
    fn copy_two_takes_the_reply_before_last() {
        let said = |role: &str, content: &str| letronna_core::chat::Message {
            role: role.into(),
            content: content.into(),
            ..Default::default()
        };
        let messages = [
            said("user", "first"),
            said("assistant", "the first answer"),
            said("user", "second"),
            said("assistant", ""),
            said("tool", "ran"),
            said("assistant", "the second answer"),
        ];
        assert_eq!(super::nth_reply(&messages, 1), Some("the second answer"));
        assert_eq!(super::nth_reply(&messages, 2), Some("the first answer"));
        assert_eq!(super::nth_reply(&messages, 3), None);
    }

    #[test]
    fn the_effort_slider_left_where_it_was_keeps_what_was_saved() {
        // On auto, saved as nothing, enter without moving stays auto rather
        // than naming an empty level.
        assert_eq!(super::slid_to("auto", "auto", ""), "auto");
        // A level another model went further with stays saved.
        assert_eq!(super::slid_to("high", "high", "max"), "max");
        assert_eq!(super::slid_to("low", "high", "max"), "low");
        assert_eq!(super::slid_to("auto", "high", "max"), "auto");
    }

    #[test]
    fn a_switch_reads_on_off_or_flips() {
        assert_eq!(super::switched("/notify", "", true), Some(false));
        assert_eq!(super::switched("/notify", "", false), Some(true));
        assert_eq!(super::switched("/notify", "off", false), Some(false));
        assert_eq!(super::switched("/notify", " on ", true), Some(true));
        assert_eq!(super::switched("/notify", "please", true), None);
    }

    #[test]
    fn an_export_is_named_after_its_session() {
        assert_eq!(
            super::file_stem("Fix the login bug!", 7),
            "fix-the-login-bug"
        );
        assert_eq!(super::file_stem("  ", 7), "session-7");
        assert_eq!(super::file_stem("修复 登录", 7), "修复-登录");
    }

    #[test]
    fn status_writes_the_home_folder_as_a_tilde() {
        let home = std::path::Path::new("/Users/someone");
        let file = home.join("Library/Application Support/Letronna/sessions/7.jsonl");
        assert_eq!(
            super::shown_path(&file, Some(home)),
            std::path::Path::new("~")
                .join("Library/Application Support/Letronna/sessions/7.jsonl")
                .display()
                .to_string()
        );
        assert_eq!(super::shown_path(home, Some(home)), "~");
        let elsewhere = std::path::Path::new("/srv/app");
        assert_eq!(super::shown_path(elsewhere, Some(home)), "/srv/app");
        assert_eq!(super::shown_path(elsewhere, None), "/srv/app");
    }

    #[test]
    fn help_puts_every_command_under_one_heading() {
        let shown: Vec<&str> = (super::help_groups().iter())
            .flat_map(|(_, entries)| entries.iter().map(|entry| entry.name))
            .collect();
        for entry in crate::bar::COMMANDS {
            let times = shown.iter().filter(|name| **name == entry.name).count();
            assert_eq!(times, 1, "{}", entry.name);
        }
    }

    #[test]
    fn help_lists_the_keys_that_search_wipe_and_switch_mode() {
        let keys: Vec<&str> = super::KEYS.iter().map(|(key, _)| *key).collect();
        for key in ["ctrl+r", "ctrl+l", "alt+m", "click", "end"] {
            assert!(keys.iter().any(|listed| listed.contains(key)), "{key}");
        }
    }

    #[test]
    fn context_lists_each_part_with_its_tokens() {
        let parts = letronna_core::chat::ContextParts {
            system: 3_200,
            tools: 9_000,
            mcp: 0,
            memory: 400,
            skills: 1_000,
            conversation: 10_400,
        };
        let rows: Vec<String> = (super::context_rows(parts).iter())
            .map(|row| crate::render::plain(row))
            .collect();
        assert_eq!(rows.len(), 6);
        assert!(
            rows[0].starts_with("system") && rows[0].contains("3.2k"),
            "{}",
            rows[0]
        );
        // Nothing gets no bar; a little gets a sliver; the most the most.
        assert_eq!(rows[2].matches('━').count(), 0);
        assert_eq!(rows[3].matches('━').count(), 1);
        assert_eq!(rows[5].matches('━').count(), 11);
        assert!(
            rows.iter()
                .all(|row| row.chars().count() == rows[0].chars().count())
        );
    }
}
