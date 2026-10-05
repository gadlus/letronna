//! The `letronna_info` tool: what Letronna holds of the user's setup in it,
//! read from its own files -- the account, the models and keys, the chat
//! apps, the schedules, the MCP servers and the rest -- so that a question
//! about any of them is answered from here and not by searching the computer.
//!
//! Nothing secret is said. A key, token, password or webhook is only said to
//! be set or not and where it is kept, and no address, chat, member, server
//! or team ID is named; nor a schedule's prompt or check, nor an MCP server's
//! command, address or error, any of which can hold a key. The account's
//! email is given only when the call asks for it.
//!
//! Everything is read from disk but the account's plan and credit, which are
//! asked of Letronna's own API once the copy kept here is older than `STALE`,
//! and taken from that copy when the API does not answer within `ASK_TIME`.
//!
//! The answer is the user's to read, so only a session they watch gets one:
//! the window, the terminal, an editor, a chat app's private chat, and their
//! helpers. A chat others read is offered no tool of this computer; a
//! scheduled run's answer is posted to every chat app set up, a webhook's
//! channel among them; and a run nobody watches, such as `-p` with its
//! output piped, prints where a script, a log or CI may show it.

use crate::auth::Account;
use crate::chat::Connection;
use crate::tools::{Tool, ToolContext};
use serde_json::{Value, json};
use std::path::Path;
use std::time::Duration;

/// The tool's name, by which the driver refuses it to a run nobody watches.
pub(crate) const LETRONNA_INFO: &str = "letronna_info";

/// Why a call is refused where someone other than the user could read it.
pub(crate) const WITHHELD: &str = "letronna_info answers only in a session the user \
     watches, not in a chat others read, a scheduled run or a run nobody watches.";

/// What a call can ask about; `all` is the first line of each of the others.
const TOPICS: [&str; 12] = [
    "all",
    "account",
    "models",
    "chat_apps",
    "schedules",
    "mcp",
    "profiles",
    "permissions",
    "settings",
    "usage",
    "version",
    "data",
];

/// How old the account's kept copy may be before it is asked for again:
/// credit moves with each hosted request, and in five minutes a second
/// question in one conversation still reads what the first one fetched.
const STALE: Duration = Duration::from_secs(5 * 60);

/// How long the account API gets before the kept copy is used: it answers
/// well within a second, and the turn waits on it.
const ASK_TIME: Duration = Duration::from_secs(5);

/// The most permission rules listed: far more than anyone writes by hand,
/// so a file of hundreds does not fill the answer.
const RULES_SHOWN: usize = 20;

/// How the account part asks Letronna's API for a fresh copy.
type Refresh = fn() -> Result<Account, String>;

/// The model's way to read Letronna's own state; it changes nothing.
pub struct LetronnaInfo;

impl Tool for LetronnaInfo {
    fn name(&self) -> &'static str {
        LETRONNA_INFO
    }
    fn description(&self) -> &'static str {
        "The user's setup in Letronna, from its own settings: account, plan and credit, models and provider keys, chat apps, schedules, MCP servers and more. Says whether a key or token is set and where, never its value."
    }
    fn parameters(&self) -> Value {
        json!({"type": "object", "properties": {
            "topic": {"type": "string", "enum": TOPICS, "description": "all gives a line of each."},
            "email": {"type": "boolean", "description": "With account: add the sign-in email, only when the user asks for it."}
        }, "required": ["topic"]})
    }
    // A scheduled run's answer is posted to every chat app set up, a
    // webhook's channel others read among them. `NOT_AVAILABLE` keeps it
    // from a run nobody answers; this, from one the window serves in Default
    // mode, with someone at its cards. Its helpers, whose calls carry
    // another session number, are refused at the call.
    fn offered(&self, connection: &Connection) -> bool {
        crate::schedule::running(connection.conversation).is_none()
    }
    fn run(&self, args: &Value, ctx: &ToolContext) -> Result<String, String> {
        let connection = ctx.caller.connection.as_deref();
        if !connection.is_some_and(|connection| read_by_the_user_alone(connection, ctx.caller.root))
        {
            return Err(WITHHELD.into());
        }
        let topic = args["topic"].as_str().unwrap_or_default().trim();
        let email = args["email"].as_bool().unwrap_or(false);
        answer(topic, email, ctx, from_the_api)
    }
}

/// Whether only the user reads what a run over `connection` for session
/// `root` gets back: someone watches it, nobody else reads it, and it is no
/// scheduled run.
// A helper's connection carries its session's watcher and chat, so this
// holds for helpers as for the session. A scheduled run the window serves
// in Default mode has someone at its cards, but its answer, a helper's
// report in it, still goes to every chat app set up.
pub(crate) fn read_by_the_user_alone(connection: &Connection, root: u64) -> bool {
    let shared = connection
        .remote
        .as_ref()
        .is_some_and(|remote| remote.shared);
    crate::prompt_rules::watched(connection) && !shared && crate::schedule::running(root).is_none()
}

/// A fresh copy of the account from Letronna's API.
fn from_the_api() -> Result<Account, String> {
    crate::auth::refresh_within(ASK_TIME)
}

/// What a call in `ctx` is told of `topic`, with the account's email when
/// `email` asks for it, a stale account fetched again through `refresh`.
fn answer(topic: &str, email: bool, ctx: &ToolContext, refresh: Refresh) -> Result<String, String> {
    let connection = ctx.caller.connection.as_deref();
    Ok(match topic {
        "all" => all(ctx, refresh),
        "account" => account(email, refresh),
        "models" => models(connection),
        "chat_apps" => chat_apps(),
        "schedules" => schedules(),
        "mcp" => mcp(),
        "profiles" => profiles(),
        "permissions" => permissions(ctx),
        "settings" => settings(),
        "usage" => usage(ctx.caller.root),
        "version" => version(),
        "data" => data(),
        "" => {
            return Err(format!(
                "`topic` is required: one of {}.",
                TOPICS.join(", ")
            ));
        }
        other => {
            return Err(format!(
                "There is no topic `{other}`: ask for one of {}.",
                TOPICS.join(", ")
            ));
        }
    })
}

/// The first line of every other topic, which each is written to sum it up.
fn all(ctx: &ToolContext, refresh: Refresh) -> String {
    let mut lines: Vec<String> = TOPICS[1..]
        .iter()
        .filter_map(|topic| {
            let told = answer(topic, false, ctx, refresh).unwrap_or_else(|why| why);
            told.lines().next().map(str::to_owned)
        })
        .collect();
    lines.push("Ask for one topic to see more of it.".into());
    lines.join("\n")
}

/// The account's plan, credit and team, and its email when `email` asks for
/// it: from the copy kept here, or from `refresh` once that is stale.
fn account(email: bool, refresh: Refresh) -> String {
    let Some(kept) = crate::auth::account() else {
        return "Not signed in to a Letronna account: /login in the terminal or Settings › \
                Account in the window signs in."
            .into();
    };
    let path = crate::auth::path();
    let mut note = None;
    let known = if stale(&path) {
        match refresh() {
            Ok(fresh) => fresh,
            // The refresh drops a token the website signed out, and says so.
            Err(why) if crate::auth::account().is_none() => return why,
            Err(why) => {
                note = Some(format!("{why} This is the copy kept here{}.", as_of(&path)));
                kept
            }
        }
    } else {
        kept
    };
    let who = match email && !known.email.trim().is_empty() {
        true => format!("Signed in as {}", known.email.trim()),
        false => "Signed in".to_owned(),
    };
    let plan = match known.plan_name() {
        "" => "plan not known yet".to_owned(),
        plan => format!("{plan} plan"),
    };
    let billed = match &known.team {
        Some(team) => format!("hosted models bill the team {}", team.name),
        None => "hosted models use this credit".to_owned(),
    };
    let mut lines = vec![format!(
        "{who}: {plan}, {} credit; {billed}.",
        known.credit()
    )];
    if let Some(team) = &known.team {
        let role = match team.role.trim() {
            "" => String::new(),
            role => format!("; the user is its {role}"),
        };
        lines.push(format!(
            "The team's pool has {}{role}.",
            dollars(team.credits)
        ));
    }
    lines.extend(note);
    lines.push(format!(
        "The account is managed at {}.",
        crate::auth::dashboard()
    ));
    lines.join("\n")
}

/// Whether the file at `path` was written longer than `STALE` ago; one whose
/// time cannot be read counts as stale, so it is asked for again.
fn stale(path: &Path) -> bool {
    let age = std::fs::metadata(path)
        .and_then(|meta| meta.modified())
        .ok()
        .and_then(|at| at.elapsed().ok());
    age.is_none_or(|age| age > STALE)
}

/// When the file at `path` was written, as " as of 09:12 on Mon 5 Oct", or
/// nothing when that cannot be read.
fn as_of(path: &Path) -> String {
    let Ok(at) = std::fs::metadata(path).and_then(|meta| meta.modified()) else {
        return String::new();
    };
    let at: chrono::DateTime<chrono::Local> = at.into();
    format!(" as of {}", at.format("%H:%M on %a %-d %b"))
}

/// Cents as dollars, as the account's own credit is shown.
fn dollars(cents: i64) -> String {
    format!("${}.{:02}", cents / 100, cents.rem_euclid(100))
}

/// The model this session and new sessions use, and which providers and
/// search services have a key, and where each is kept.
fn models(connection: Option<&Connection>) -> String {
    let settings = crate::store::current_settings();
    let mut lines = Vec::new();
    if let Some(connection) = connection.filter(|connection| !connection.model.is_empty()) {
        let on = gateway_at(&connection.endpoint, &settings.custom_endpoint)
            .unwrap_or("a server set for this run");
        lines.push(format!(
            "This session: {} on {on}{}.",
            connection.model,
            effort(&connection.effort)
        ));
    }
    let model = match settings.model.trim() {
        "" => "no model chosen",
        model => model,
    };
    lines.push(format!(
        "New sessions: {model} on {}{}.",
        crate::gateway::gateway(&settings.gateway).name,
        effort(&settings.effort)
    ));
    if !settings.helper_model.trim().is_empty() {
        lines.push(format!("Helpers: {}.", settings.helper_model.trim()));
    }
    if !settings.side_model.trim().is_empty() {
        lines.push(format!(
            "Summaries and checks: {}.",
            settings.side_model.trim()
        ));
    }
    if !settings.fallbacks.is_empty() {
        lines.push(format!(
            "Fallbacks, in order: {}.",
            settings.fallbacks.join(", ")
        ));
    }
    let keys = file_json(&crate::store::keys_path());
    let key = |slot: &str| {
        let name = format!("{}{slot}", crate::vault::KEYS);
        kept(keys.get(slot).and_then(Value::as_str), "keys.json", &name)
    };
    let signed_in = crate::auth::account().is_some();
    let providers: Vec<String> = (crate::gateway::GATEWAYS.iter())
        .map(|gateway| {
            let state = match gateway.name {
                crate::gateway::LETRONNA if signed_in => "signed in".to_owned(),
                crate::gateway::LETRONNA => "not signed in".to_owned(),
                "Custom" if settings.custom_endpoint.trim().is_empty() => "not set up".to_owned(),
                "Custom" => match key("Custom") {
                    Some(at) => format!("address set, key set {at}"),
                    None => "address set, no key".to_owned(),
                },
                _ if !gateway.needs_key => "needs no key".to_owned(),
                name => match key(name) {
                    Some(at) => format!("key set {at}"),
                    None => "no key".to_owned(),
                },
            };
            format!("{} {state}", gateway.name)
        })
        .collect();
    lines.push(format!("Providers: {}.", providers.join("; ")));
    let choice = crate::search::choice();
    let saved = crate::store::load_keys();
    let services: Vec<String> = (crate::search::SERVICES.iter())
        .map(|service| {
            let (status, _, _) = crate::search::status(service.name, &choice, &saved);
            let status = status.to_lowercase();
            let keyed = !service.keyless && !service.self_hosted();
            match keyed.then(|| key(service.name)).flatten() {
                Some(at) => format!("{} {status}, key {at}", service.name),
                None => format!("{} {status}", service.name),
            }
        })
        .collect();
    lines.push(format!("Web search: {}.", services.join("; ")));
    // Any other slot, such as where a remote browser is reached.
    let mut others: Vec<String> = saved
        .keys()
        .filter(|slot| {
            let named = |name: &str| name == slot.as_str();
            !(crate::gateway::GATEWAYS.iter()).any(|gateway| named(gateway.name))
                && !(crate::search::SERVICES.iter()).any(|service| named(service.name))
        })
        .filter_map(|slot| Some(format!("{slot} {}", key(slot)?)))
        .collect();
    others.sort();
    if !others.is_empty() {
        lines.push(format!("Also set: {}.", others.join("; ")));
    }
    lines.push(format!(
        "Keys and tokens are kept {}.",
        crate::vault::place()
    ));
    lines.extend(crate::vault::trouble());
    lines.join("\n")
}

/// The gateway whose address `endpoint` is, Custom's being `custom`.
fn gateway_at(endpoint: &str, custom: &str) -> Option<&'static str> {
    let bare = |url: &str| url.trim().trim_end_matches('/').to_owned();
    (crate::gateway::GATEWAYS.iter())
        .map(|gateway| gateway.name)
        .find(|name| {
            let base = crate::gateway::base_url(name, custom);
            !base.trim().is_empty() && bare(&base) == bare(endpoint)
        })
}

/// How hard a model is told to think, as words after its name.
fn effort(effort: &str) -> String {
    match effort.trim() {
        "" => String::new(),
        effort => format!(", effort {effort}"),
    }
}

/// Which chat apps are set up and how, which of their tokens are set and
/// where they are kept, and where a scheduled run reports.
fn chat_apps() -> String {
    let path = crate::messaging::path();
    if !path.exists() {
        return "No chat app is set up: the window's Messaging page sets up Telegram, \
                Discord, Slack and email."
            .into();
    }
    let config = crate::messaging::load();
    let file = file_json(&path);
    let token = |pointer: &str| {
        let (_, name) = (crate::messaging::SECRETS.iter()).find(|(at, _)| *at == pointer)?;
        kept(
            file.pointer(pointer).and_then(Value::as_str),
            "messaging.json",
            name,
        )
    };
    let on = |enabled: bool| if enabled { "on" } else { "off" };
    let set = |what: &str, place: Option<String>| match place {
        Some(at) => format!("{what} set {at}"),
        None => format!("no {what}"),
    };
    let paired = |paired: bool| if paired { "paired" } else { "not paired" };
    let tools = |allowed: bool| match allowed {
        true => "its messages may use tools that change things",
        false => "its messages may only read and search",
    };
    let enabled: Vec<&str> = [
        (config.telegram.enabled, "Telegram"),
        (config.discord.enabled, "Discord"),
        (config.slack.enabled, "Slack"),
        (config.email.enabled, "email"),
    ]
    .into_iter()
    .filter_map(|(enabled, name)| enabled.then_some(name))
    .collect();
    let first = match enabled.is_empty() {
        true => "No chat app is on.".to_owned(),
        false => format!("Chat apps on: {}.", enabled.join(", ")),
    };
    let mut lines = vec![format!("{first} {}", reports_to(&config))];
    let telegram = &config.telegram;
    lines.push(format!(
        "Telegram: {}; {}; {}; {}.",
        on(telegram.enabled),
        set("bot token", token("/telegram/token")),
        paired(telegram.chat.is_some()),
        tools(telegram.allow_tools)
    ));
    let discord = &config.discord;
    let servers = match discord.servers.len() {
        0 => "DMs only".to_owned(),
        1 => "DMs and 1 server".to_owned(),
        n => format!("DMs and {n} servers"),
    };
    let owner = match discord.owner.trim().is_empty() {
        true => "no owner's user ID",
        false => "owner's user ID set",
    };
    lines.push(format!(
        "Discord: {}; {}; {}; {owner}; {servers}; {}.",
        on(discord.enabled),
        set("webhook", token("/discord/webhook")),
        set("bot token", token("/discord/bot_token")),
        tools(discord.allow_tools)
    ));
    let slack = &config.slack;
    lines.push(format!(
        "Slack: {}; {}; {}; {}; {}.",
        on(slack.enabled),
        set("app token", token("/slack/app_token")),
        set("bot token", token("/slack/bot_token")),
        paired(!slack.owner.trim().is_empty()),
        tools(slack.allow_tools)
    ));
    let email = &config.email;
    let addressed = match email.smtp_host.trim().is_empty() || email.to.trim().is_empty() {
        true => "server or address missing",
        false => "server and address set",
    };
    lines.push(format!(
        "Email: {}; {addressed}; {}.",
        on(email.enabled),
        set("app password", token("/email/password"))
    ));
    if let Some(minutes) = config.phone_cards {
        lines.push(format!(
            "A card left {minutes} minutes in the window also goes to the owner's private chat."
        ));
    }
    lines.join("\n")
}

/// Where a scheduled run's answer goes now, as a sentence.
fn reports_to(config: &crate::messaging::Config) -> String {
    let places = crate::messaging::reporting(config);
    match places.is_empty() {
        true => "No chat app takes a scheduled run's answer now.".into(),
        false => format!(
            "A scheduled run's answer is posted to: {}.",
            places.join(", ")
        ),
    }
}

/// The schedules: when each runs, in which mode and who made it, and where
/// a run's answer goes.
fn schedules() -> String {
    let list = match crate::schedule::load(&crate::chat::data_dir()) {
        Ok(list) => list,
        Err(why) => return why,
    };
    let reports = reports_to(&crate::messaging::load());
    if list.is_empty() {
        return format!("There are no schedules.\n{reports}");
    }
    let on = list.iter().filter(|job| job.enabled).count();
    let kind = if list.len() == 1 {
        "schedule"
    } else {
        "schedules"
    };
    let mut lines = vec![format!("{} {kind}, {on} on.", list.len())];
    let now = chrono::Local::now();
    for job in &list {
        let who = if job.by_model {
            "added by you"
        } else {
            "made by the user"
        };
        let done =
            matches!(job.trigger, crate::schedule::Trigger::Once { at } if job.last_run >= at);
        let state = match (job.enabled, done) {
            (_, true) => ", done",
            (false, false) => ", off",
            (true, false) => "",
        };
        let quiet = if job.quiet { ", only with news" } else { "" };
        let check = if job.check.is_some() {
            ", after a check command"
        } else {
            ""
        };
        let next = match crate::schedule::next_due(job, now) {
            Some(at) => format!(", next at {}", local_time(at)),
            None => String::new(),
        };
        lines.push(format!(
            "#{} \"{}\": {}, {} mode, {who}{quiet}{check}{state}{next}",
            job.id,
            job.name,
            job.trigger.label(),
            job.mode.name()
        ));
    }
    lines.push(reports);
    lines.join("\n")
}

/// Epoch seconds as this computer's time, as "09:00 on Tue 6 Oct".
fn local_time(at: u64) -> String {
    let at = i64::try_from(at).ok();
    let at = at.and_then(|at| chrono::TimeZone::timestamp_opt(&chrono::Local, at, 0).single());
    at.map_or_else(
        || "an unknown time".to_owned(),
        |at| at.format("%H:%M on %a %-d %b").to_string(),
    )
}

/// Each MCP server, how it is reached and how it stands.
fn mcp() -> String {
    use crate::mcp::State;
    if let Some(why) = crate::mcp::unreadable() {
        return why;
    }
    let servers = crate::mcp::status();
    if servers.is_empty() {
        return "No MCP servers are set up: `letronna mcp add` adds one.".into();
    }
    let listed: Vec<String> = servers
        .iter()
        .map(|server| {
            let reached = match server.transport {
                "http" => "by URL",
                _ => "run here",
            };
            let tools = if server.tools == 1 { "tool" } else { "tools" };
            let state = match server.state {
                State::Starting => "starting".to_owned(),
                State::Connected => format!("connected, {} {tools}", server.tools),
                State::Retrying => "could not start, trying again".to_owned(),
                State::Failed => "failed".to_owned(),
                State::Disabled => "switched off".to_owned(),
                State::NeedsAuth => "waiting for a sign-in".to_owned(),
            };
            format!("{} ({reached}) {state}", server.name)
        })
        .collect();
    let mut lines = vec![format!("MCP servers: {}.", listed.join("; "))];
    // Its error is the server's own words, for the user's screen.
    let failing =
        (servers.iter()).any(|server| matches!(server.state, State::Failed | State::Retrying));
    if failing {
        lines.push(
            "/mcp in the terminal or Capabilities › MCP in the window shows why one failed.".into(),
        );
    }
    lines.join("\n")
}

/// The profile in use and the others there are.
fn profiles() -> String {
    let names: Vec<String> = (crate::profiles::list().into_iter())
        .map(|profile| profile.name)
        .collect();
    format!(
        "Profile in use: {}, of {}.\nEach has its own sessions, keys, settings, skills and \
         memory; Profile in the window's account menu switches them.",
        crate::profiles::active(),
        names.join(", ")
    )
}

/// The session's mode and new sessions', the user's and the project's
/// permission rules, and the tools switched off.
fn permissions(ctx: &ToolContext) -> String {
    let settings = crate::store::current_settings();
    let mut lines = Vec::new();
    if let Some(connection) = ctx.caller.connection.as_deref() {
        lines.push(format!(
            "This session's mode: {} ({}).",
            connection.mode.name(),
            connection.mode.about()
        ));
    }
    lines.push(format!(
        "New sessions start in {} mode.",
        settings.mode.name()
    ));
    match crate::permissions::load(&crate::permissions::user_file()) {
        Ok(rules) => lines.extend(listed_rules(&rules, "permissions.json")),
        Err(why) => lines.push(why),
    }
    if let Some(project) = ctx.project() {
        match crate::permissions::project_rules(project) {
            Ok(rules) if rules.is_empty() => {}
            Ok(rules) => lines.extend(listed_rules(&rules, crate::permissions::PROJECT_FILE)),
            Err(why) => lines.push(why),
        }
    }
    let off = crate::tools::turned_off();
    if !off.is_empty() {
        lines.push(format!("Tools switched off: {}.", off.join(", ")));
    }
    lines.join("\n")
}

/// `rules` from the file `file` as lines, the first `RULES_SHOWN` of them.
fn listed_rules(rules: &[crate::permissions::Rule], file: &str) -> Vec<String> {
    use crate::permissions::Effect;
    if rules.is_empty() {
        return vec![format!("No rules in {file}.")];
    }
    let kind = if rules.len() == 1 { "rule" } else { "rules" };
    let mut lines = vec![format!("{} {kind} in {file}:", rules.len())];
    for rule in rules.iter().take(RULES_SHOWN) {
        let effect = match rule.effect {
            Effect::Allow => "allow",
            Effect::Ask => "ask about",
            Effect::Deny => "deny",
        };
        let calls = match rule.whole() {
            true => "every call".to_owned(),
            false => format!("`{}`", rule.pattern.trim()),
        };
        let place = match &rule.project {
            Some(folder) => format!(" in {}", folder.display()),
            None => String::new(),
        };
        lines.push(format!("- {effect} {}: {calls}{place}", rule.tool));
    }
    if rules.len() > RULES_SHOWN {
        lines.push(format!("- and {} more", rules.len() - RULES_SHOWN));
    }
    lines
}

/// The choices in settings.json a person makes, in plain words.
fn settings() -> String {
    use crate::memory::{Saving, Scope};
    let settings = crate::store::current_settings();
    let on = |yes: bool| if yes { "on" } else { "off" };
    let or = |value: &str, none: &'static str| match value.trim() {
        "" => none.to_owned(),
        value => value.to_owned(),
    };
    let saving = match settings.memory_saving {
        Saving::WhenAsked => "only when the user asks",
        Saving::Ask => "after the user's yes to each",
        Saving::Auto => "whenever the model sees fit",
    };
    let scope = match settings.memory_scope {
        Scope::Auto => "everywhere",
        Scope::Project => "for the project only",
        Scope::Chat => "for the chat only",
    };
    let updates = match settings.manual_updates {
        true => "only when asked",
        false => "automatic",
    };
    let sessions = match settings.keep_days {
        0 => "sessions are kept forever".to_owned(),
        days => format!("sessions untouched for {days} days are deleted"),
    };
    [
        format!(
            "New sessions: {} mode, effort {}, reply style {}.",
            settings.mode.name(),
            or(&settings.effort, "left to the model"),
            or(&settings.style, "none")
        ),
        format!(
            "Thinking shown: {}; the gateway's own web search: {}; DuckDuckGo with no search \
             key: {}.",
            on(settings.thinking),
            on(settings.search),
            on(settings.keyless_search)
        ),
        format!(
            "Memory is saved {saving}, {scope}; memory review: {}.",
            on(settings.memory_review)
        ),
        format!(
            "Voice replies: {}; notifications: {}; one browser for every session: {}; computer \
             use: {}.",
            on(settings.voice),
            on(settings.notifications),
            on(settings.browser_shared),
            on(settings.computer_use)
        ),
        format!("Updates: {updates}; {sessions}."),
        "The rest is in settings.json; the letronna skill says what each setting does.".into(),
    ]
    .join("\n")
}

/// What the model calls of session `session` have taken so far.
fn usage(session: u64) -> String {
    if session == 0 {
        return "This call is in no saved session, so there is no usage to add up.".into();
    }
    let mut totals = crate::usage::Totals::default();
    // Since the end of time: no other session's file, only this one's.
    let spent = crate::store::spent(u64::MAX, session);
    for call in spent.iter().filter(|call| call.session == session) {
        totals.add(&call.usage);
    }
    format!(
        "This session so far: {}.\n/usage in the terminal adds up the last days and models.",
        totals.line()
    )
}

/// This build's version and system, and any newer one found or downloaded.
fn version() -> String {
    let mut lines = vec![format!(
        "Letronna {} on {} {}.",
        crate::update::VERSION,
        std::env::consts::OS,
        std::env::consts::ARCH
    )];
    lines.push(match (crate::update::staged(), crate::update::seen()) {
        (Some(staged), _) => format!("{staged} is downloaded and goes in at the next start."),
        (None, Some(seen)) => format!("{seen} is out: `letronna update` installs it."),
        (None, None) if !crate::update::enabled() => "This build does not update itself.".into(),
        (None, None) => "The last check found nothing newer.".into(),
    });
    if crate::store::current_settings().manual_updates {
        lines.push("Updates are looked for only when asked.".into());
    }
    lines.join("\n")
}

/// Where Letronna keeps its data, its settings, sessions, logs and keys.
fn data() -> String {
    let dir = crate::chat::data_dir();
    let mut lines = vec![format!("Data folder: {}.", dir.display())];
    if dir != crate::chat::base_dir() {
        lines.push(format!(
            "That is the {} profile's; the default profile's is {}.",
            crate::profiles::active(),
            crate::chat::base_dir().display()
        ));
    }
    lines.push(format!(
        "Settings: {}. Sessions: {}. Logs: {}.",
        crate::store::settings_path().display(),
        crate::store::sessions_dir().display(),
        crate::diag::logs(&dir).display()
    ));
    lines.push(format!(
        "Keys and tokens are kept {}.",
        crate::vault::place()
    ));
    if let Some(folder) = crate::chat::fallback_folder() {
        lines.push(format!(
            "Work done with no project folder goes in {}.",
            folder.display()
        ));
    }
    lines.push("The letronna skill says what each file in the data folder holds.".into());
    lines.join("\n")
}

/// A JSON file of Letronna's as it is on disk, or null.
fn file_json(path: &Path) -> Value {
    std::fs::read(path)
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

/// Where a secret is kept: in `file`, when its copy there `in_file` holds
/// one, or else in the vault under `name`; None when it is set nowhere.
fn kept(in_file: Option<&str>, file: &str, name: &str) -> Option<String> {
    if in_file.is_some_and(|secret| !secret.trim().is_empty()) {
        return Some(format!("in {file}"));
    }
    match crate::vault::get(name) {
        Ok(Some(secret)) if !secret.trim().is_empty() => Some("in the vault".into()),
        Ok(_) => None,
        // Listed there, so kept, though the vault would not give it now.
        Err(_) => Some("in the vault, which would not give it now".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chat::Remote;
    use crate::store::tests::{DATA_DIR, Scratch};
    use std::sync::MutexGuard;
    use std::time::SystemTime;

    /// The session the calls are made in.
    const SESSION: u64 = 9_800_101;

    /// What no answer may hold: the word every key, token, password,
    /// webhook and address seeded below carries, and the IDs, email, name
    /// and mail server, which carry no such word.
    const SECRETS: [&str; 11] = [
        "SECRET",
        "777123456",
        "888777666",
        "5551234",
        "888123456789",
        "999888777666",
        "U0SLACKOWNER",
        "team-id-42",
        "ada@private.example",
        "Ada Example",
        "mailhost",
    ];

    /// A data folder of this test's own, held from other tests, with an
    /// empty vault on this thread.
    fn data(name: &str) -> (MutexGuard<'static, ()>, Scratch) {
        let held = DATA_DIR.lock().unwrap_or_else(|e| e.into_inner());
        let dir = Scratch::new(name);
        unsafe { std::env::set_var("AGENT_DATA_DIR", &*dir) };
        crate::vault::tests::fake();
        (held, dir)
    }

    /// A session at the window on OpenRouter.
    fn window() -> Connection {
        Connection {
            endpoint: "https://openrouter.ai/api/v1".into(),
            model: "test/model".into(),
            tools: true,
            human: true,
            conversation: SESSION,
            ..Default::default()
        }
    }

    /// A chat app's chat with its owner, or one others read when `shared`.
    fn chat(shared: bool) -> Connection {
        Connection {
            remote: Some(Remote {
                place: "a Telegram chat".into(),
                shared,
                history: None,
            }),
            ..window()
        }
    }

    /// The account kept here: Pro, $12.34, billing a team's pool.
    fn sign_in(dir: &Path) {
        let account = r#"{"name": "Ada Example", "email": "ada@private.example",
            "token": "device-token-SECRET-0123456789", "since": 1, "plan": "pro",
            "credits": 1234, "team": {"id": "team-id-42", "name": "Acme",
            "credits": 5000, "role": "admin"}}"#;
        std::fs::write(dir.join("account.json"), account).unwrap();
    }

    /// Makes the account kept here ten minutes old, past `STALE`.
    fn age_account() {
        let ago = SystemTime::now() - Duration::from_secs(10 * 60);
        let file = std::fs::File::options()
            .write(true)
            .open(crate::auth::path())
            .unwrap();
        file.set_modified(ago).unwrap();
    }

    /// Fails the test if the account is asked for again.
    fn never() -> Result<Account, String> {
        panic!("the API was asked though the kept copy is fresh");
    }

    /// A setup with every part filled in, a secret in each field that holds
    /// one: keys in the vault and in keys.json, every chat app, a schedule
    /// whose prompt and check hold a key, a custom address and a SearXNG one
    /// that carry keys, a rule, and a session with two model calls.
    fn set_up(dir: &Path) {
        sign_in(dir);
        crate::store::save_key("OpenAI", Some("sk-live-SECRET-openai-0123456789")).unwrap();
        crate::store::save_key("Brave", Some("brave-SECRET-key-0123456789")).unwrap();
        crate::store::save_key("Browser", Some("wss://browser.example/?token=SECRET")).unwrap();
        // One left in its file, as a vault that refused it leaves it.
        let mut keys = file_json(&dir.join("keys.json"))
            .as_object()
            .cloned()
            .unwrap_or_default();
        keys.insert("OpenRouter".into(), "or-SECRET-in-file-0123456789".into());
        std::fs::write(dir.join("keys.json"), Value::Object(keys).to_string()).unwrap();
        let settings = json!({"endpoint": "https://openrouter.ai/api/v1", "model": "test/model",
            "gateway": "OpenRouter", "custom_endpoint": "https://my.example/v1?key=SECRET-custom",
            "effort": "high", "fallbacks": ["OpenAI:gpt-5"]});
        std::fs::write(dir.join("settings.json"), settings.to_string()).unwrap();
        let search = r#"{"service": "Brave",
            "addresses": {"SearXNG": "http://searx.example/?token=SECRET-searx"}}"#;
        std::fs::write(dir.join("search.json"), search).unwrap();
        let mut chats: crate::messaging::Config = serde_json::from_value(json!({
            "telegram": {"enabled": true, "token": "7123456789:AAH-SECRET-telegram",
                         "chat": 777123456, "owner": 888777666},
            "discord": {"enabled": true,
                        "webhook": "https://discord.com/api/webhooks/5551234/SECRET-hook",
                        "bot_token": "SECRET-discord-bot", "owner": "888123456789",
                        "servers": ["999888777666"]},
            "slack": {"enabled": true, "app_token": "xapp-SECRET-slack-app",
                      "bot_token": "xoxb-SECRET-slack-bot", "owner": "U0SLACKOWNER"},
            "email": {"enabled": true, "smtp_host": "smtp.mailhost.example", "port": 465,
                      "username": "mailuser-SECRET", "password": "SECRET-mail-password",
                      "from": "from@mailhost.example", "to": "owner@mailhost.example"},
            "phone_cards": 5
        }))
        .unwrap();
        // The tokens to the vault, as the window saves them.
        crate::messaging::save(&chats).unwrap();
        // And the webhook kept in the file.
        chats.discord.webhook = "https://discord.com/api/webhooks/5551234/SECRET-hook".into();
        let mut file: Value =
            serde_json::from_slice(&std::fs::read(crate::messaging::path()).unwrap()).unwrap();
        file["discord"]["webhook"] = chats.discord.webhook.clone().into();
        std::fs::write(crate::messaging::path(), file.to_string()).unwrap();
        let job = crate::schedule::Schedule {
            id: 1,
            name: "Daily hello".into(),
            prompt: "Post to https://discord.com/api/webhooks/1/SECRET-prompt".into(),
            workspace: None,
            trigger: crate::schedule::Trigger::DailyAt {
                hour: 9,
                minute: 0,
                weekdays_only: false,
            },
            enabled: true,
            // Armed as it was added, so it waits for its time.
            last_run: crate::chat::now(),
            mode: crate::gate::Mode::Default,
            by_model: true,
            quiet: false,
            check: Some("curl -H 'Authorization: Bearer SECRET-check' x".into()),
        };
        let jobs = serde_json::to_vec(&[job]).unwrap();
        std::fs::write(dir.join("schedules.json"), jobs).unwrap();
        let servers = r#"{"mcpServers": {"files": {"command": "npx",
            "args": ["--token", "SECRET-mcp-arg"], "env": {"API_KEY": "SECRET-mcp-env"}}}}"#;
        std::fs::write(dir.join("mcp.json"), servers).unwrap();
        let rules = r#"[{"tool": "run_command", "pattern": "git push", "effect": "deny"}]"#;
        std::fs::write(dir.join("permissions.json"), rules).unwrap();
        std::fs::create_dir_all(dir.join("sessions")).unwrap();
        let session = [
            format!(r#"{{"type":"head","id":{SESSION},"created":100}}"#),
            r#"{"type":"message","role":"user","content":"hi"}"#.into(),
            r#"{"type":"message","role":"assistant","content":"a","model":"m","tokens":40,"prompt_tokens":700,"at":200}"#.into(),
            r#"{"type":"message","role":"assistant","content":"b","model":"m","tokens":60,"prompt_tokens":500,"at":300}"#.into(),
        ];
        let file = dir.join("sessions").join(format!("{SESSION}.jsonl"));
        std::fs::write(file, session.join("\n")).unwrap();
    }

    /// What a call in a window session is told of `topic`.
    fn told(topic: &str) -> String {
        let ctx = ToolContext::of(&window());
        answer(topic, false, &ctx, never).unwrap()
    }

    #[test]
    fn each_topic_answers_from_letronnas_own_files() {
        let (_held, dir) = data("info-topics");
        set_up(&dir);
        // A server that failed to start, as /mcp would list it.
        crate::mcp::pretend("info-broken", crate::mcp::State::Failed);

        let account = told("account");
        let models = told("models");
        let chat_apps = told("chat_apps");
        let schedules = told("schedules");
        let mcp = told("mcp");
        let profiles = told("profiles");
        let permissions = told("permissions");
        let settings = told("settings");
        let usage = told("usage");
        let version = told("version");
        let data = told("data");
        let all = told("all");
        unsafe { std::env::remove_var("AGENT_DATA_DIR") };

        assert!(
            account.starts_with(
                "Signed in: Pro plan, $12.34 credit; hosted models bill the team Acme.\n\
                 The team's pool has $50.00; the user is its admin.\n"
            ),
            "{account}"
        );
        for line in [
            "This session: test/model on OpenRouter.",
            "New sessions: test/model on OpenRouter, effort high.",
            "Fallbacks, in order: OpenAI:gpt-5.",
            "Providers: Letronna signed in; OpenAI key set in the vault; Anthropic no key; \
             OpenRouter key set in keys.json; Ollama needs no key; Custom address set, no key.",
            "Web search: Brave in use, key in the vault; Tavily not set up; Exa not set up; \
             SearXNG saved, not in use; DuckDuckGo not in use.",
            "Also set: Browser in the vault.",
            "Keys and tokens are kept in the test vault under \"Letronna-test\", listed in \
             vault.json.",
        ] {
            assert!(models.contains(line), "{line}\n{models}");
        }
        for line in [
            "Chat apps on: Telegram, Discord, Slack, email. A scheduled run's answer is posted \
             to: Discord (webhook), Discord (bot DM), Slack, Telegram, email.",
            "Telegram: on; bot token set in the vault; paired; its messages may only read and \
             search.",
            "Discord: on; webhook set in messaging.json; bot token set in the vault; owner's \
             user ID set; DMs and 1 server; its messages may only read and search.",
            "Slack: on; app token set in the vault; bot token set in the vault; paired;",
            "Email: on; server and address set; app password set in the vault.",
            "A card left 5 minutes in the window also goes to the owner's private chat.",
        ] {
            assert!(chat_apps.contains(line), "{line}\n{chat_apps}");
        }
        assert!(
            schedules.starts_with("1 schedule, 1 on.\n#1 "),
            "{schedules}"
        );
        assert!(
            schedules.ends_with(
                "\nA scheduled run's answer is posted to: Discord (webhook), \
                 Discord (bot DM), Slack, Telegram, email."
            ),
            "{schedules}"
        );
        assert!(
            schedules.contains(
                "#1 \"Daily hello\": Daily at 09:00, default mode, added by you, after a check \
                 command, next at 09:00 on "
            ),
            "{schedules}"
        );
        assert!(mcp.contains("info-broken (run here) failed"), "{mcp}");
        assert!(mcp.contains("/mcp in the terminal"), "{mcp}");
        assert_eq!(
            profiles,
            "Profile in use: Default, of Default.\nEach has its own sessions, keys, settings, \
             skills and memory; Profile in the window's account menu switches them."
        );
        for line in [
            "This session's mode: default (asks before writing or running).",
            "New sessions start in default mode.",
            "1 rule in permissions.json:\n- deny run_command: `git push`",
        ] {
            assert!(permissions.contains(line), "{line}\n{permissions}");
        }
        assert!(
            settings.starts_with("New sessions: default mode, effort high, reply style none."),
            "{settings}"
        );
        assert!(
            usage.starts_with("This session so far: 2 calls · 1.2k in · 100 out.\n"),
            "{usage}"
        );
        assert!(
            version.starts_with(&format!("Letronna {} on ", crate::update::VERSION)),
            "{version}"
        );
        assert!(
            data.starts_with(&format!("Data folder: {}.", dir.display())),
            "{data}"
        );
        // A line of each, the first each gives alone.
        let firsts: Vec<&str> = [
            &account,
            &models,
            &chat_apps,
            &schedules,
            &mcp,
            &profiles,
            &permissions,
            &settings,
            &usage,
            &version,
            &data,
        ]
        .iter()
        .map(|told| told.lines().next().unwrap())
        .collect();
        assert_eq!(
            all,
            format!(
                "{}\nAsk for one topic to see more of it.",
                firsts.join("\n")
            )
        );
    }

    #[test]
    fn no_key_token_password_webhook_address_or_id_is_in_any_answer() {
        let (_held, dir) = data("info-secrets");
        set_up(&dir);
        let ctx = ToolContext::of(&window());
        let mut told: Vec<(String, String)> = TOPICS
            .iter()
            .map(|topic| {
                (
                    topic.to_string(),
                    answer(topic, false, &ctx, never).unwrap(),
                )
            })
            .collect();
        // And whole, through the gate and the scrub, as the model gets it.
        for topic in TOPICS {
            let args = json!({"topic": topic}).to_string();
            let ran = crate::tools::run(LETRONNA_INFO, &args, &ctx).unwrap();
            told.push((format!("{topic} run"), ran));
        }
        // With the email asked for, it is the one thing more.
        let with_email = answer("account", true, &ctx, never).unwrap();
        unsafe { std::env::remove_var("AGENT_DATA_DIR") };

        for (topic, told) in &told {
            for secret in SECRETS {
                assert!(!told.contains(secret), "{topic} told {secret}:\n{told}");
            }
        }
        assert!(
            with_email.starts_with("Signed in as ada@private.example: Pro plan"),
            "{with_email}"
        );
        let without = with_email.replace("ada@private.example", "");
        for secret in SECRETS {
            assert!(!without.contains(secret), "{secret}:\n{with_email}");
        }
    }

    #[test]
    fn a_stale_account_is_asked_for_again_and_a_fresh_one_is_not() {
        let (_held, dir) = data("info-refresh");
        sign_in(&dir);
        // Fresh: `never` would fail the test.
        let fresh = account(false, never);
        age_account();
        let asked = account(false, || {
            Ok(Account {
                plan: "team".into(),
                credits: 999,
                ..Account::default()
            })
        });
        unsafe { std::env::remove_var("AGENT_DATA_DIR") };

        assert!(
            fresh.starts_with("Signed in: Pro plan, $12.34 credit;"),
            "{fresh}"
        );
        assert!(
            asked.starts_with("Signed in: Team plan, $9.99 credit; hosted models use this credit."),
            "{asked}"
        );
    }

    #[test]
    fn an_account_api_that_does_not_answer_leaves_the_kept_copy() {
        let (_held, dir) = data("info-refresh-fails");
        sign_in(&dir);
        age_account();
        let told = account(false, || {
            Err("Could not reach the Letronna account service.".into())
        });
        unsafe { std::env::remove_var("AGENT_DATA_DIR") };

        assert!(
            told.starts_with("Signed in: Pro plan, $12.34 credit; hosted models bill the team"),
            "{told}"
        );
        assert!(
            told.contains(
                "\nCould not reach the Letronna account service. This is the copy kept here \
                 as of "
            ),
            "{told}"
        );
    }

    #[test]
    fn an_account_the_website_signed_out_reads_as_signed_out() {
        let (_held, dir) = data("info-signed-out");
        sign_in(&dir);
        age_account();
        // As the refresh does on a 401: the token goes, and it says so.
        let told = account(false, || {
            std::fs::remove_file(crate::auth::path()).unwrap();
            Err("This computer was signed out. /login to sign in again.".into())
        });
        let signed_out = account(false, never);
        unsafe { std::env::remove_var("AGENT_DATA_DIR") };

        assert_eq!(
            told,
            "This computer was signed out. /login to sign in again."
        );
        assert!(signed_out.starts_with("Not signed in"), "{signed_out}");
    }

    #[test]
    fn it_is_offered_where_only_the_user_reads_the_answer() {
        let (_held, dir) = data("info-offered");
        let listed = |connection: &Connection| {
            crate::tools::specs(connection)
                .iter()
                .any(|spec| spec["function"]["name"] == LETRONNA_INFO)
        };
        let job = crate::schedule::Schedule {
            id: 1,
            name: "Daily".into(),
            prompt: "Say hello.".into(),
            workspace: None,
            trigger: crate::schedule::Trigger::Every { minutes: 60 },
            enabled: true,
            last_run: 0,
            mode: crate::gate::Mode::Default,
            by_model: false,
            quiet: false,
            check: None,
        };
        let scheduled = 9_800_102;
        let _listed = crate::schedule::Listed::new(scheduled, &job);
        // An editor over ACP connects as the window does; a `-p` run with
        // nobody there is left out by the driver.
        let cases = [
            ("the window", window(), true),
            (
                "the terminal",
                Connection {
                    plain_terminal: true,
                    ..window()
                },
                true,
            ),
            ("a chat with its owner", chat(false), true),
            (
                "plan mode",
                Connection {
                    plan: true,
                    ..window()
                },
                true,
            ),
            ("a chat others read", chat(true), false),
            (
                "a scheduled run's own turn",
                Connection {
                    human: false,
                    conversation: scheduled,
                    ..window()
                },
                false,
            ),
            (
                "a scheduled run the window serves in Default mode",
                Connection {
                    conversation: scheduled,
                    ..window()
                },
                false,
            ),
        ];
        let offered: Vec<(&str, bool, bool)> = (cases.iter())
            .map(|(what, connection, offered)| (*what, listed(connection), *offered))
            .collect();
        std::fs::write(
            dir.join("settings.json"),
            r#"{"endpoint": "", "model": "m", "disabled_tools": ["letronna_info"]}"#,
        )
        .unwrap();
        let switched_off = listed(&window());
        unsafe { std::env::remove_var("AGENT_DATA_DIR") };

        for (what, listed, offered) in offered {
            assert_eq!(listed, offered, "{what}");
        }
        assert!(!switched_off);
    }

    #[test]
    fn a_call_is_answered_only_where_the_user_alone_reads_it() {
        let (_held, dir) = data("info-refused");
        sign_in(&dir);
        let call = |connection: Connection| {
            let args = json!({"topic": "account"}).to_string();
            crate::tools::run(LETRONNA_INFO, &args, &ToolContext::of(&connection))
        };
        // A helper of the session `root`, with a call number of its own.
        let helper = |root: u64| {
            let mut ctx = ToolContext::of(&Connection {
                conversation: root,
                ..window()
            });
            ctx.conversation = 9_800_103;
            ctx.caller.depth = 1;
            let args = json!({"topic": "account"}).to_string();
            crate::tools::run(LETRONNA_INFO, &args, &ctx)
        };
        let window_told = call(window());
        let chat_told = call(chat(false));
        let helper_told = helper(SESSION);
        let shared = call(chat(true));
        // As in `-p` with its output piped, or a scheduled run and its helpers.
        let unwatched = call(Connection {
            human: false,
            ..window()
        });
        // The window serves a Default mode scheduled run with someone at its
        // cards, but the run's answer goes to every chat app.
        let job = crate::schedule::Schedule {
            id: 1,
            name: "Daily".into(),
            prompt: "Say hello.".into(),
            workspace: None,
            trigger: crate::schedule::Trigger::Every { minutes: 60 },
            enabled: true,
            last_run: 0,
            mode: crate::gate::Mode::Default,
            by_model: false,
            quiet: false,
            check: None,
        };
        let scheduled = 9_800_104;
        let listed = crate::schedule::Listed::new(scheduled, &job);
        let in_a_run = call(Connection {
            conversation: scheduled,
            ..window()
        });
        let helping_a_run = helper(scheduled);
        drop(listed);
        let unknown = crate::tools::run(
            LETRONNA_INFO,
            r#"{"topic": "keys"}"#,
            &ToolContext::of(&window()),
        );
        unsafe { std::env::remove_var("AGENT_DATA_DIR") };

        for told in [window_told, chat_told, helper_told] {
            assert!(told.unwrap().starts_with("Signed in: Pro plan"));
        }
        let shared = shared.unwrap_err();
        assert!(
            shared.contains("other people can read this chat"),
            "{shared}"
        );
        assert_eq!(unwatched.unwrap_err(), WITHHELD);
        let in_a_run = in_a_run.unwrap_err();
        assert!(
            in_a_run.contains("not available in this session"),
            "{in_a_run}"
        );
        assert_eq!(helping_a_run.unwrap_err(), WITHHELD);
        let unknown = unknown.unwrap_err();
        assert!(
            unknown.starts_with("There is no topic `keys`: ask for one of all, account"),
            "{unknown}"
        );
    }
}
