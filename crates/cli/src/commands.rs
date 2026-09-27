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
            render::blank();
            render::line(&render::dim("The guide: https://letronna.ai/docs"));
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
            format_args!("${}.{:02}", team.credits / 100, team.credits.rem_euclid(100))
        )),
        None => render::line(&render::dim("team    none · hosted models use your own credit · /team to pick one")),
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
    let found = runner.bar.busy("loading your teams", letronna_core::auth::teams);
    let teams = match found {
        Ok(teams) => teams,
        Err(error) => {
            render::blank();
            render::line(&render::red(&error));
            return;
        }
    };
    let current = letronna_core::auth::account().and_then(|a| a.team).map(|t| t.id);
    let mark = |on: bool| if on { "● " } else { "  " };
    let mut labels = vec![format!("{}Personal — your own credit", mark(current.is_none()))];
    labels.extend(teams.iter().map(|t| format!("{}{} — team pool", mark(current.as_deref() == Some(t.id.as_str())), t.name)));
    let options: Vec<&str> = labels.iter().map(String::as_str).collect();
    let detail = if teams.is_empty() {
        vec!["You are not in a team yet. Create or join one at letronna.ai/dashboard/team.".to_owned()]
    } else {
        Vec::new()
    };
    let Some(picked) = runner.bar.choose("Bill hosted models to", detail, &options) else {
        return;
    };
    let id = picked.checked_sub(1).and_then(|i| teams.get(i)).map(|t| t.id.clone());
    let done = runner.bar.busy("switching", move || letronna_core::auth::use_team(id.as_deref()));
    render::blank();
    match done {
        Ok(account) => {
            match &account.team {
                Some(team) => render::line(&format!("Hosted models now bill {}.", render::bold(&team.name))),
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
    open_in_browser(&pending.url);

    match runner
        .bar
        .busy("waiting for the browser", move || pending.finish())
    {
        Ok(account) => {
            render::said(&render::blue("›"), &render::bold(&account.name));
            let plan = account.plan_name();
            render::line(&render::dim(&if plan.is_empty() {
                "Signed in.".to_owned()
            } else {
                format!("Signed in · {plan} · {} credit", account.credit())
            }));
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
    let (mut models, errors) = runner.bar.busy("asking every gateway what it has", move || {
        letronna_core::gateway::all_models(&keys, &custom)
    });
    for error in &errors {
        render::line(&render::red(error));
    }
    if models.is_empty() {
        if errors.is_empty() {
            render::line(&render::dim("No gateway offered any models. Sign in with /login, or add a provider with `letronna setup`."));
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
        .busy("asking every gateway what it has", move || gateway::all_models(&k, &c));

    // The list sits in the box with its choices, not in the conversation.
    // Letronna is the account, not a gateway to add or remove: /login and
    // /logout look after it, so it has a status here and no choice.
    let mut rows = Vec::new();
    let mut choices: Vec<(String, &'static str, &'static str)> = Vec::new();
    for g in GATEWAYS {
        let count = models.iter().filter(|(n, _)| n == g.name).count();
        let (status, good, set_up) = gateway::status(g.name, &keys, &custom, count);
        let status = if g.name == LETRONNA && !set_up { format!("{status} · /login") } else { status };
        rows.push(format!("{} {:<11} {status}", if good { "●" } else { "○" }, g.name));
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
            render::line(&render::dim(&format!("{name} removed. Its models leave /model.")));
        }
        _ => {
            if gateway::gateway(name).base.is_empty() {
                let url = runner.bar.ask(&format!("{name} base URL, like http://localhost:8000/v1"), false).unwrap_or_default();
                if !url.trim().is_empty() {
                    runner.store.settings.custom_endpoint = url.trim().trim_end_matches('/').to_owned();
                }
            }
            let hint = if gateway::gateway(name).needs_key { format!("{name} API key") } else { format!("{name} API key (optional)") };
            let key = runner.bar.ask(&hint, true).unwrap_or_default();
            if !key.trim().is_empty() {
                keys.insert(gateway::key_slot(name).to_owned(), key.trim().to_owned());
            }
            render::blank();
            render::line(&render::dim(&format!("{name} saved. /model now lists its models too.")));
        }
    }
    if let Err(error) = store::save_keys(&keys) {
        render::line(&render::red(&format!("Could not save the key: {error}")));
    }
    runner.persist();
}
