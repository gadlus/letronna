//! A chat with a model on this computer: where its requests go, how they
//! are asked, what it may reach, and when it stands in for a hosted one.
//!
//! A session on this computer keeps one address, `BASE`, whatever port the
//! server has this time, so the facts learned about the model, its quirks
//! and its cooling down stay put across restarts. Only at the moment a
//! request is sent is the address swapped for the live server's and its key
//! added: `wake` starts or swaps the server for a chat's request and holds
//! its turn until the answer ends, and `reach` does the same for the calls
//! beside a chat -- its name, a summary, the judge, a commit message --
//! which reuse the request's turn when they come inside it.
//!
//! Nothing a chat here says leaves the computer: every side call goes to
//! the same model, the router never hands it to another gateway, and the
//! only tools it is offered that reach out are a web search, while online,
//! for a model the eval trusts with one (and web_fetch for an agent).

use super::catalog::{self, Entry, Tools};
use super::{fit, machine, runtime, shelf};
use crate::cancel::Cancel;
use crate::chat::Connection;
use crate::failure::{Failure, FailureKind};
use crate::store::Settings;
use serde_json::{Value, json};
use std::cell::RefCell;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// What settings, sessions and fallbacks call this computer's models.
pub const NAME: &str = "This computer";

/// The address a chat on this computer is kept under. Nothing listens on
/// port 0, so a request that went around `wake` fails at once instead of
/// reaching something else; and it is localhost, so every check that keeps
/// a chat's words on this computer counts it as here.
pub const BASE: &str = "http://localhost:0/v1";

/// The longest reply asked for in a chat or a search: a small model given
/// more rambles on.
const REPLY: u64 = 2048;
/// An agent's, which writes whole files in one call.
const AGENT_REPLY: u64 = 8192;

/// Whether `endpoint` is this computer's models.
pub fn at(endpoint: &str) -> bool {
    endpoint.trim().trim_end_matches('/') == BASE
}

/// The model `connection` asks for on this computer.
pub fn entry_of(connection: &Connection) -> Option<&'static Entry> {
    if !at(&connection.endpoint) {
        return None;
    }
    catalog::entry(connection.model.trim())
}

/// What a list or a switch calls `connection`'s model when it runs here.
pub fn shown(connection: &Connection) -> Option<&'static str> {
    entry_of(connection).map(|entry| entry.name)
}

/// The longest reply `entry` is asked for.
fn reply_cap(entry: &Entry) -> u64 {
    if entry.tools == Tools::Agent {
        AGENT_REPLY
    } else {
        REPLY
    }
}

/// What the models list says of `entry` started with `context`: what the
/// window sizes requests by, what a reply may take, and whether pictures
/// are sent, which they are once its vision pack is here.
fn listing(entry: &Entry, context: u64) -> Value {
    let mut inputs = vec!["text"];
    if shelf::eyes_ready(entry) {
        inputs.push("image");
    }
    json!({
        "id": entry.id,
        "owned_by": entry.maker,
        "context_length": context,
        "max_output_tokens": reply_cap(entry),
        "input_modalities": inputs,
        "supported_parameters": [],
    })
}

/// Tell the models list what each model here holds, so a chat is sized
/// right before its server ever starts.
pub(crate) fn remember_all(ids: &[&str]) {
    let machine = machine::cached();
    let list: Vec<Value> = ids
        .iter()
        .filter_map(|id| catalog::entry(id))
        .map(|entry| {
            let context = machine
                .as_ref()
                .map_or(fit::CONTEXT, |m| fit::fit(entry, m, false).context);
            listing(entry, context)
        })
        .collect();
    crate::models::remember(BASE, &list);
}

/// The live server a request on this thread was sent to, while it runs.
struct Here {
    url: String,
    key: String,
    standing_in: bool,
}

thread_local! {
    static HERE: RefCell<Option<Here>> = const { RefCell::new(None) };
}

/// A chat's request on this computer, its server ready and its turn held.
pub(crate) struct Woken {
    /// The connection as this model is asked: its tools and prompt.
    pub connection: Connection,
    _turn: runtime::Turn,
}

impl Drop for Woken {
    fn drop(&mut self) {
        HERE.with_borrow_mut(Option::take);
    }
}

/// Start or swap in the server for `asked` when it is on this computer,
/// saying so through `notes`, and hold its turn; nothing when it is not.
/// `primary` is the session's own, so a stand-in for a hosted model is
/// known; `pictures` whether the chat sends any.
pub(crate) fn wake(
    asked: &Connection,
    primary: &Connection,
    pictures: bool,
    notes: &dyn Fn(String),
    cancel: &Cancel,
) -> Result<Option<Woken>, Failure> {
    if !at(&asked.endpoint) {
        return Ok(None);
    }
    let settings = |why: String| Failure::new(FailureKind::Settings, why);
    let Some(entry) = entry_of(asked) else {
        return Err(settings(format!(
            "{} is not a model Letronna runs on this computer.",
            asked.model.trim()
        )));
    };
    if shelf::ready(entry).is_none() {
        return Err(settings(missing(entry)));
    }
    if pictures && entry.eyes.is_none() && !at(&primary.endpoint) {
        // Standing in, the pictures are left out rather than the answer.
    } else if pictures && entry.eyes.is_none() {
        return Err(settings(format!(
            "{} can't read photos. Pick a model that can.",
            entry.name
        )));
    }
    let photos = pictures && shelf::eyes_ready(entry);
    if !runtime::serving(entry.id) {
        notes(format!("Loading {}…", entry.name));
    }
    let live = live(entry, photos, notes, cancel).map_err(|why| {
        let kind = if why == "Stopped." {
            FailureKind::Stopped
        } else {
            FailureKind::Server
        };
        Failure::new(kind, why)
    })?;
    crate::models::remember(BASE, &[listing(entry, live.context)]);
    let connection = shaped(asked, entry, entry.tools != Tools::Chat && online());
    HERE.with_borrow_mut(|here| {
        *here = Some(Here {
            url: live.endpoint,
            key: live.key,
            standing_in: !at(&primary.endpoint),
        });
    });
    Ok(Some(Woken {
        connection,
        _turn: live.turn,
    }))
}

/// What a chat on a model no longer here is told.
fn missing(entry: &Entry) -> String {
    format!(
        "{} is not on this computer any more. Get it again or pick another.",
        entry.name
    )
}

#[cfg(not(test))]
fn live(
    entry: &'static Entry,
    photos: bool,
    notes: &dyn Fn(String),
    cancel: &Cancel,
) -> Result<runtime::Live, String> {
    runtime::ensure(entry, photos, notes, cancel)
}

/// No test starts llama-server: one stands its own server in.
#[cfg(test)]
fn live(
    _: &'static Entry,
    _: bool,
    _: &dyn Fn(String),
    _: &Cancel,
) -> Result<runtime::Live, String> {
    let serving = tests::SERVING.lock().unwrap_or_else(|e| e.into_inner());
    let Some((endpoint, key, context)) = serving.clone() else {
        return Err("No server in tests.".into());
    };
    Ok(runtime::Live {
        endpoint,
        key,
        context,
        turn: runtime::Turn::stand_in(),
    })
}

/// `asked` as `entry` is asked: a short prompt and at most a web search for
/// a model trusted with less than Letronna's tools, or for an agent whose
/// window they would fill; never the gateway's own web search, which
/// llama-server does not know.
fn shaped(asked: &Connection, entry: &Entry, online: bool) -> Connection {
    let short = || Connection {
        tools: asked.tools && online && entry.tools != Tools::Chat,
        search: false,
        rare_on_demand: false,
        effort: String::new(),
        system: Some(
            asked
                .system
                .clone()
                .unwrap_or_else(|| prompt(entry, entry.tools != Tools::Chat && online).into()),
        ),
        ..asked.clone()
    };
    if entry.tools != Tools::Agent || asked.system.is_some() {
        return short();
    }
    let agent = Connection {
        search: false,
        rare_on_demand: true,
        effort: String::new(),
        ..asked.clone()
    };
    let report = crate::chat::context_report(&agent, &[], 0);
    let fixed = report.parts.total() - report.parts.conversation;
    if report.window > 0 && fixed * 2 <= report.window {
        agent
    } else {
        short()
    }
}

/// The system prompt of a model here trusted with less than Letronna's
/// tools: who it is and where, the date, and how to search when it may.
pub(crate) fn prompt(entry: &Entry, searches: bool) -> String {
    let today = chrono::Local::now().format("%A, %-d %B %Y").to_string();
    words(entry.name, entry.maker, &today, searches)
}

/// `prompt`'s words, for a given model and day.
pub(crate) fn words(name: &str, maker: &str, today: &str, searches: bool) -> String {
    let mut text = format!(
        "You are Letronna, a personal assistant. You run on this computer as {name}, \
         an open model by {maker}: what is said here stays here. Be warm, plain and \
         brief; use Markdown when it helps. Answer in the language you are written to.\n\n\
         Today is {today}."
    );
    if searches {
        text.push_str(
            "\n\nFor anything current or on the internet (prices, news, schedules, \
             availability, places), call search_web, then answer from the results and \
             link your sources. Never invent facts, prices or links.",
        );
    }
    text
}

/// Whether this computer can reach the internet, as far as can be told
/// without sending anything: the system has a route out (a UDP connect
/// asks the routing table and sends no packet) and no request to the web
/// failed to connect lately.
fn online() -> bool {
    #[cfg(test)]
    if let Some(online) = *tests::ONLINE.lock().unwrap_or_else(|e| e.into_inner()) {
        return online;
    }
    let routed = |addr: &str| {
        let bind = if addr.starts_with('[') {
            "[::]:0"
        } else {
            "0.0.0.0:0"
        };
        std::net::UdpSocket::bind(bind).is_ok_and(|socket| socket.connect(addr).is_ok())
    };
    (routed("1.1.1.1:53") || routed("[2606:4700:4700::1111]:53")) && !crate::web::out_of_reach()
}

/// `url`, a request's address under `BASE`, at the live server this
/// thread's request was sent to.
pub(crate) fn address(url: String) -> String {
    HERE.with_borrow(|here| match here {
        Some(here) => swap(&url, &here.url),
        None => url,
    })
}

fn swap(url: &str, live: &str) -> String {
    match url.strip_prefix(BASE) {
        Some(rest) => format!("{live}{rest}"),
        None => url.to_owned(),
    }
}

/// `request` with the live server's key, when this thread has one.
pub(crate) fn signed(
    request: reqwest::blocking::RequestBuilder,
) -> reqwest::blocking::RequestBuilder {
    HERE.with_borrow(|here| match here {
        Some(here) => request.bearer_auth(&here.key),
        None => request,
    })
}

/// Whether this thread's request stands in for a hosted model that could
/// not be reached, whose summarizer could not be either.
pub(crate) fn standing_in() -> bool {
    HERE.with_borrow(|here| here.as_ref().is_some_and(|here| here.standing_in))
}

/// Where a call beside a chat goes, with the key and turn it needs.
pub(crate) struct Reached {
    pub url: String,
    key: Option<String>,
    _turn: Option<runtime::Turn>,
}

impl Reached {
    /// `request` with the server's key, when it is one here.
    pub(crate) fn sign(
        &self,
        request: reqwest::blocking::RequestBuilder,
    ) -> reqwest::blocking::RequestBuilder {
        match &self.key {
            Some(key) => request.bearer_auth(key),
            None => request,
        }
    }
}

/// Where a call beside `connection`'s chat goes, `url` being its address
/// as validated: unchanged off this computer; here, the live server, inside
/// a request's turn or with one of its own. `idle_only` asks nothing of a
/// server that is busy or not running, as naming a chat should not.
pub(crate) fn reach(
    connection: &Connection,
    url: String,
    idle_only: bool,
) -> Result<Reached, String> {
    if !at(&connection.endpoint) {
        return Ok(Reached {
            url,
            key: None,
            _turn: None,
        });
    }
    if let Some((live, key)) =
        HERE.with_borrow(|here| here.as_ref().map(|h| (h.url.clone(), h.key.clone())))
    {
        return Ok(Reached {
            url: swap(&url, &live),
            key: Some(key),
            _turn: None,
        });
    }
    let entry = entry_of(connection)
        .ok_or_else(|| format!("{} is not on this computer.", connection.model.trim()))?;
    if idle_only && !(runtime::idle() && serving(entry)) {
        return Err(format!("{} is busy or not loaded.", entry.name));
    }
    let live = live(
        entry,
        runtime::serving_photos(entry.id),
        &|_| {},
        &Cancel::default(),
    )?;
    Ok(Reached {
        url: swap(&url, &live.endpoint),
        key: Some(live.key),
        _turn: Some(live.turn),
    })
}

#[cfg(not(test))]
fn serving(entry: &Entry) -> bool {
    runtime::serving(entry.id)
}

#[cfg(test)]
fn serving(_: &Entry) -> bool {
    tests::SERVING
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .is_some()
}

/// Shape a request body for `connection`'s model when it is one here: its
/// sampling, its thinking off, its reply held short, no effort level, one
/// system message and no empty tool arguments, which some templates refuse.
pub(crate) fn shape_body(connection: &Connection, body: &mut Value) {
    let Some(entry) = entry_of(connection) else {
        return;
    };
    entry.shape(body, reply_cap(entry));
    let Some(messages) = body.get_mut("messages").and_then(Value::as_array_mut) else {
        return;
    };
    // Qwen's templates, Bonsai's among them, refuse a system message after
    // the first; what one says still goes, as the user's.
    for message in messages.iter_mut().skip(1) {
        if message["role"] == "system" {
            message["role"] = "user".into();
        }
    }
    for message in messages.iter_mut() {
        let Some(calls) = message.get_mut("tool_calls").and_then(Value::as_array_mut) else {
            continue;
        };
        for call in calls {
            let arguments = &mut call["function"]["arguments"];
            if arguments.as_str().is_some_and(|a| a.trim().is_empty()) {
                *arguments = "{}".into();
            }
        }
    }
}

/// `failure`, said as the computer having slept when it did since `since`:
/// the server may have died with the graphics chip, and the next request
/// starts it again. What had arrived of the answer stands.
pub(crate) fn after_sleep(failure: Failure, since: (std::time::SystemTime, Instant)) -> Failure {
    if !runtime::slept(since.0, since.1) {
        return failure;
    }
    let mut slept = Failure::new(failure.kind, "Cut off when the computer slept. Ask again.");
    slept.stands = failure.stands;
    slept
}

/// The tools a chat here is never offered and may not call: each would send
/// its words elsewhere -- a browser, a media service, a chat app, a plugin,
/// a wallet, the cloud, an MCP server.
fn kept_off() -> Vec<String> {
    use crate::toolset::Family;
    let mut names: Vec<String> = [
        Family::Browser,
        Family::Media,
        Family::Messaging,
        Family::Plugins,
    ]
    .into_iter()
    .flat_map(Family::tools)
    .collect();
    names.extend(
        [
            "edit_image",
            "letronna_info",
            "notify",
            "paid_fetch",
            "wallet",
            "wallet_send",
            "cloud_app_power",
            "cloud_apps",
            "cloud_task",
            "skill_manage",
            crate::mcp_find::FIND_TOOLS,
            crate::mcp_find::CALL_MCP_TOOL,
        ]
        .map(str::to_owned),
    );
    names
}

/// Whether a chat on `connection` may be offered and call `tool`.
pub(crate) fn lets(connection: &Connection, tool: &str) -> bool {
    if !at(&connection.endpoint) {
        return true;
    }
    if connection.system.is_some() {
        return tool == "search_web";
    }
    !crate::mcp_find::is_mcp(tool) && !kept_off().iter().any(|name| name == tool)
}

/// Why a chat here may not call `tool`, for the model to read.
pub(crate) fn refused(tool: &str) -> String {
    format!(
        "`{tool}` is not available here: this chat runs on this computer, and that tool \
         would send its words elsewhere. Carry on without it."
    )
}

/// The model here to stand in for a hosted one that cannot be reached:
/// the best downloaded one that runs here, when Settings let it.
pub(crate) fn stand_in(primary: &Connection, settings: &Settings) -> Option<Connection> {
    if !settings.local_stand_in || at(&primary.endpoint) {
        return None;
    }
    let machine = machine::cached().unwrap_or_else(machine::now);
    let entry = catalog::CATALOG
        .iter()
        .filter(|entry| shelf::ready(entry).is_some() && fit::fits(entry, &machine))
        .max_by_key(|entry| entry.rank)?;
    Some(Connection {
        endpoint: BASE.into(),
        key: String::new(),
        model: entry.id.into(),
        effort: String::new(),
        search: false,
        ..primary.clone()
    })
}

/// When this computer last stood in for a hosted model it could not reach.
fn stood_in() -> &'static Mutex<Option<Instant>> {
    static STOOD_IN: Mutex<Option<Instant>> = Mutex::new(None);
    &STOOD_IN
}

/// How long after standing in a request is still taken here without
/// trying the hosted model, while that cools down.
const STANDING: Duration = crate::fallback::COOLDOWN;

/// Whether `link` may be asked after the failures so far: a model here
/// standing in for a hosted one only once that could not be reached, or
/// while it cools down after being out of reach. Never the other way.
pub(crate) fn may_stand_in(
    primary: &Connection,
    link: &Connection,
    failed: Option<FailureKind>,
) -> bool {
    if !at(&link.endpoint) || at(&primary.endpoint) {
        return true;
    }
    let mut stood = stood_in().lock().unwrap_or_else(|e| e.into_inner());
    match failed {
        Some(FailureKind::Network) => {
            *stood = Some(Instant::now());
            true
        }
        Some(_) => false,
        None => stood.is_some_and(|at| at.elapsed() < STANDING),
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::store::tests::{DATA_DIR, Scratch};

    /// The server a test stands in: its address, key and context.
    pub(crate) static SERVING: Mutex<Option<(String, String, u64)>> = Mutex::new(None);
    /// Whether a test says this computer is online.
    pub(crate) static ONLINE: Mutex<Option<bool>> = Mutex::new(None);
    /// Held by a test that sets `SERVING` or `ONLINE`, or stands in for a
    /// hosted model, which every test shares.
    pub(crate) static SEAMS: Mutex<()> = Mutex::new(());

    /// Forget that this computer stood in for a hosted model lately.
    pub(crate) fn forget_standing() {
        *stood_in().lock().unwrap_or_else(|e| e.into_inner()) = None;
    }

    /// A scratch data folder with `id` on its shelf, as a file of the right
    /// size, the lock held while it stands.
    pub(crate) fn shelved(
        name: &str,
        ids: &[&str],
    ) -> (std::sync::MutexGuard<'static, ()>, Scratch) {
        let lock = DATA_DIR.lock().unwrap_or_else(|e| e.into_inner());
        let scratch = Scratch::new(name);
        // SAFETY: tests that set the data folder hold DATA_DIR.
        unsafe { std::env::set_var("AGENT_DATA_DIR", scratch.as_path()) };
        shelve(ids);
        (lock, scratch)
    }

    /// Put `ids` on the shelf of the data folder set now, as files of the
    /// right size.
    pub(crate) fn shelve(ids: &[&str]) {
        for id in ids {
            let entry = catalog::entry(id).unwrap();
            let path = shelf::path(entry, &entry.file);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::File::create(&path)
                .unwrap()
                .set_len(entry.file.size)
                .unwrap();
        }
    }

    fn here(model: &str) -> Connection {
        Connection {
            endpoint: BASE.into(),
            model: model.into(),
            tools: true,
            ..Default::default()
        }
    }

    #[test]
    fn the_address_of_this_computer_passes_every_check_and_reads_as_nearby() {
        let connection = here("bonsai-2-4b");
        assert_eq!(
            crate::chat::validate(&connection).as_deref(),
            Ok("http://localhost:0/v1/chat/completions")
        );
        assert!(crate::web::local(&reqwest::Url::parse(BASE).unwrap()));
        assert!(at("http://localhost:0/v1/"));
        assert!(!at("http://localhost:11434/v1"));
        let started = Instant::now();
        let client = reqwest::blocking::Client::builder()
            .no_proxy()
            .build()
            .unwrap();
        assert!(client.get(format!("{BASE}/models")).send().is_err());
        assert!(started.elapsed() < Duration::from_secs(2));
    }

    #[test]
    fn a_model_not_downloaded_is_a_settings_failure_and_starts_nothing() {
        let (_lock, _scratch) = shelved("reach-missing", &[]);
        let notes = Mutex::new(Vec::new());
        let failure = wake(
            &here("bonsai-2-4b"),
            &here("bonsai-2-4b"),
            false,
            &|n| notes.lock().unwrap().push(n),
            &Cancel::default(),
        )
        .err()
        .unwrap();
        assert_eq!(failure.kind, FailureKind::Settings);
        assert!(
            failure
                .to_string()
                .contains("not on this computer any more"),
            "{failure}"
        );
        assert!(notes.lock().unwrap().is_empty());
    }

    #[test]
    fn each_kind_of_model_is_offered_what_it_can_use() {
        let chat = catalog::entry("gemma-4-e4b").unwrap();
        let shaped_chat = shaped(&here(chat.id), chat, true);
        assert!(!shaped_chat.tools && !shaped_chat.search);
        let system = shaped_chat.system.unwrap();
        assert!(system.contains("Gemma 4 · E4B") && !system.contains("search_web"));
        let search = catalog::entry("bonsai-2-4b").unwrap();
        let online = shaped(&here(search.id), search, true);
        assert!(online.tools && online.system.unwrap().contains("call search_web"));
        assert!(lets(&online_with_system(search), "search_web"));
        assert!(!lets(&online_with_system(search), "read_file"));
        let offline = shaped(&here(search.id), search, false);
        assert!(!offline.tools && !offline.system.unwrap().contains("search_web"));
    }

    fn online_with_system(entry: &Entry) -> Connection {
        shaped(&here(entry.id), entry, true)
    }

    #[test]
    fn an_agent_here_keeps_no_tool_that_sends_its_words_elsewhere() {
        let agent = here("bonsai-2-4b");
        for kept in [
            "read_file",
            "edit_file",
            "run_command",
            "search_web",
            "web_fetch",
        ] {
            assert!(lets(&agent, kept), "{kept}");
        }
        for off in [
            "browse",
            "use_page",
            "make_image",
            "wallet_send",
            "notify",
            "mcp__github__search",
        ] {
            assert!(!lets(&agent, off), "{off}");
        }
        let hosted = Connection {
            endpoint: "https://api.example.com/v1".into(),
            ..agent
        };
        assert!(lets(&hosted, "browse"));
    }

    #[test]
    fn every_tool_kept_off_this_computer_is_a_tool() {
        let every: Vec<String> = crate::tools::all()
            .iter()
            .map(|t| t.name().to_owned())
            .collect();
        let builtin = ["find_tools", "call_mcp_tool"];
        for name in kept_off() {
            assert!(
                every.contains(&name)
                    || builtin.contains(&name.as_str())
                    || cfg!(not(feature = "workflows")),
                "{name} is not a tool"
            );
        }
    }

    #[test]
    fn a_body_for_this_computer_carries_its_sampling_and_one_system_message() {
        let mut body = json!({
            "model": "bonsai-2-4b",
            "max_tokens": 16_000,
            "reasoning_effort": "high",
            "messages": [
                {"role": "system", "content": "You are Letronna."},
                {"role": "user", "content": "Hi"},
                {"role": "assistant", "content": "", "tool_calls": [
                    {"id": "1", "type": "function", "function": {"name": "search_web", "arguments": ""}}
                ]},
                {"role": "system", "content": "The user stopped the last reply."},
            ],
        });
        shape_body(&here("bonsai-2-4b"), &mut body);
        assert_eq!(body["top_k"], 20);
        assert_eq!(body["chat_template_kwargs"]["enable_thinking"], false);
        assert_eq!(body["reasoning_format"], "auto");
        assert_eq!(body["max_tokens"], 2048);
        assert!(body.get("reasoning_effort").is_none());
        assert_eq!(body["messages"][3]["role"], "user");
        assert_eq!(
            body["messages"][2]["tool_calls"][0]["function"]["arguments"],
            "{}"
        );
        let mut hosted = json!({"max_tokens": 16_000, "reasoning_effort": "high"});
        shape_body(
            &Connection {
                endpoint: "https://api.example.com/v1".into(),
                model: "bonsai-2-4b".into(),
                ..Default::default()
            },
            &mut hosted,
        );
        assert_eq!(
            hosted,
            json!({"max_tokens": 16_000, "reasoning_effort": "high"})
        );
    }

    #[test]
    fn a_model_here_stands_in_only_once_the_hosted_one_cannot_be_reached() {
        let _seams = SEAMS.lock().unwrap_or_else(|e| e.into_inner());
        let hosted = Connection {
            endpoint: "https://api.letronna.ai/v1".into(),
            model: "auto".into(),
            ..Default::default()
        };
        let local = here("bonsai-2-4b");
        *stood_in().lock().unwrap() = None;
        assert!(!may_stand_in(&hosted, &local, Some(FailureKind::Quota)));
        assert!(!may_stand_in(&hosted, &local, None));
        assert!(may_stand_in(&hosted, &local, Some(FailureKind::Network)));
        // While the hosted model cools down after that, requests come here.
        assert!(may_stand_in(&hosted, &local, None));
        // Never the other way, and other fallbacks as before.
        assert!(may_stand_in(&local, &hosted, Some(FailureKind::Quota)));
        *stood_in().lock().unwrap() = None;
    }

    #[test]
    fn standing_in_takes_the_best_model_here_that_runs_and_heeds_the_setting() {
        let (_lock, _scratch) = shelved("reach-stand-in", &["bonsai-2-4b", "bonsai-2-9b"]);
        let hosted = Connection {
            endpoint: "https://api.letronna.ai/v1".into(),
            model: "auto".into(),
            effort: "high".into(),
            ..Default::default()
        };
        let mut settings = Settings::default();
        let local = stand_in(&hosted, &settings).unwrap();
        assert!(at(&local.endpoint));
        assert!(local.effort.is_empty());
        // Both run on any machine Letronna knows of; the better is taken.
        assert_eq!(local.model, "bonsai-2-9b");
        settings.local_stand_in = false;
        assert!(stand_in(&hosted, &settings).is_none());
        settings.local_stand_in = true;
        assert!(stand_in(&local, &settings).is_none());
    }

    #[test]
    fn a_side_call_inside_a_request_reuses_its_server_and_one_outside_needs_it_idle() {
        let _seams = SEAMS.lock().unwrap_or_else(|e| e.into_inner());
        *SERVING.lock().unwrap() = None;
        let connection = here("bonsai-2-4b");
        let reached = reach(&connection, format!("{BASE}/chat/completions"), true);
        // Not serving here: naming leaves the first line.
        assert!(reached.is_err());
        HERE.with_borrow_mut(|here| {
            *here = Some(Here {
                url: "http://127.0.0.1:5555/v1".into(),
                key: "k".into(),
                standing_in: false,
            });
        });
        let reached = reach(&connection, format!("{BASE}/chat/completions"), true).unwrap();
        assert_eq!(reached.url, "http://127.0.0.1:5555/v1/chat/completions");
        assert_eq!(address(format!("{BASE}/chat/completions")), reached.url);
        HERE.with_borrow_mut(Option::take);
        let hosted = Connection {
            endpoint: "https://api.example.com/v1".into(),
            ..connection
        };
        let reached = reach(
            &hosted,
            "https://api.example.com/v1/chat/completions".into(),
            true,
        )
        .unwrap();
        assert_eq!(reached.url, "https://api.example.com/v1/chat/completions");
    }

    #[test]
    fn the_short_prompt_says_where_it_runs_and_when_it_may_search() {
        let plain = words(
            "Test Model 4B",
            "Test Lab",
            "Thursday, 22 October 2026",
            false,
        );
        assert!(
            plain.contains("You run on this computer as Test Model 4B, an open model by Test Lab")
        );
        assert!(plain.ends_with("Today is Thursday, 22 October 2026."));
        let searching = words(
            "Test Model 4B",
            "Test Lab",
            "Thursday, 22 October 2026",
            true,
        );
        assert!(searching.contains("call search_web"));
    }
}
