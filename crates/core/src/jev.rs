//! Jev, a small decision model, ranks the tools `find_tools` searches.
//!
//! Matching a search's words to tools' names finds a tool only when the
//! model guesses the words its name and description use: "ping the team"
//! never reaches a tool called send_message. Jev, TypeSafe's System One
//! model, writes no prose. Given the search and every tool it may find, a
//! few words on each, it gives the chance that each one does what was
//! asked, and the chance that any tool is needed at all, in one call of
//! about a second that reads a few thousand tokens. Over a catalogue of
//! 1,215 tools OpenHuman measured 22.5% right first by words and 62% with
//! Jev, and one needless call in 31 tool-less requests rather than 26.
//!
//! The tools left out of a request -- Letronna's less used ones in a lean
//! run, and the MCP servers' past their budget -- nearly always fit in the
//! one question of 255 options Jev takes, so every one is judged by what it
//! does, with no shortlist by words to miss the right one. Past that, the
//! tools the words rank highest go in.
//!
//! Where it is asked is `tool_search` in the settings. "auto", the default,
//! asks through the user's Letronna account, or the address and key set
//! for Jev, only when the chat itself goes to a hosted model: a session on
//! a model on this computer or network sends nothing out for it. "jev" asks
//! wherever it can be reached, and "names" never asks. Only the search's
//! own words and the tools' names and descriptions are sent. Whatever goes
//! wrong -- no way to reach it, a slow or refused answer, one that cannot
//! be read -- leaves the ranking to `find_tools`' own, by the words a search
//! shares with each tool, and is noted in the diagnostics log once for each
//! kind of trouble.

use crate::chat::Connection;
use serde_json::{Map, Value, json};
use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

/// The key store's slot for a Jev key of the user's own.
pub const KEY_SLOT: &str = "Jev";

/// The model asked when the settings name none: TypeSafe's latest.
const MODEL: &str = "jev-latest";

/// TypeSafe's own address, for a key of the user's own with none set.
const TYPESAFE: &str = "https://api.typesafe.ai/v1/systemone";

/// The most options one question to Jev may have, `none` among them.
const MOST_OPTIONS: usize = 255;

/// How much of a tool's description Jev is shown: enough to say what it
/// does, short enough that a whole catalogue stays a few thousand tokens.
const DESCRIPTION_CHARS: usize = 200;

/// A tool Jev gives less of a chance than this is not handed back.
const LEAST_CHANCE: f64 = 0.05;

/// How long a search waits for Jev before ranking by words instead: a
/// second or so is usual, and a search should never stall a turn.
const DEADLINE: Duration = Duration::from_secs(3);

/// How many answers are kept, so a search made again costs nothing.
const REMEMBERED: usize = 64;

/// The option that says no listed tool does it.
const NONE: &str = "none";

/// What Jev made of a search.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Judged {
    /// The tools it gives a fair chance of doing what was asked, best first.
    Ranked(Vec<String>),
    /// No listed tool does it, as it judged.
    Nothing,
}

/// Where Jev is asked, with what key and under what model name.
#[derive(Debug, PartialEq)]
struct Route {
    url: String,
    key: String,
    model: String,
}

/// What Jev makes of `query` among `tools`, each a name and description,
/// for a search on `connection`; None when it is not to be asked or gave
/// no answer that could be read, which leaves the ranking to the words.
pub(crate) fn rank(connection: &Connection, query: &str, tools: &[(&str, &str)]) -> Option<Judged> {
    let query = query.trim();
    if query.is_empty() || tools.is_empty() {
        return None;
    }
    let route = route(connection)?;
    let body = request(&route.model, query, tools);
    let key = remembered_as(&route, &body);
    if let Some(judged) = answers().lock().ok()?.get(&key) {
        return Some(judged.clone());
    }
    let judged = match ask(&route, &body) {
        Ok(answer) => read(&answer),
        Err(why) => Err(why),
    };
    match judged {
        Ok(judged) => {
            if let Ok(mut answers) = answers().lock() {
                if answers.len() >= REMEMBERED {
                    answers.clear();
                }
                answers.insert(key, judged.clone());
            }
            Some(judged)
        }
        Err(why) => {
            noted(why);
            None
        }
    }
}

/// The way to Jev for a search on `connection`, as the settings and keys
/// have it, or None where it is not asked.
fn route(connection: &Connection) -> Option<Route> {
    let (settings, _) = crate::store::settings();
    let keys = crate::store::load_keys();
    let account = crate::auth::account().map(|account| account.token);
    route_of(
        connection,
        &settings.tool_search,
        &settings.jev_endpoint,
        &settings.jev_model,
        keys.get(KEY_SLOT).map(String::as_str),
        account.as_deref(),
    )
}

/// `route` from the settings' `mode`, `endpoint` and `model`, the user's
/// own Jev `key` and the signed-in `account`'s token.
fn route_of(
    connection: &Connection,
    mode: &str,
    endpoint: &str,
    model: &str,
    key: Option<&str>,
    account: Option<&str>,
) -> Option<Route> {
    match mode.trim() {
        "names" => return None,
        // Asked anywhere it can be reached.
        "jev" => {}
        // Only where the chat goes out to a hosted model anyway.
        _ if !hosted(&connection.endpoint) => return None,
        _ => {}
    }
    let key = key.map(str::trim).filter(|key| !key.is_empty());
    let model = Some(model.trim())
        .filter(|model| !model.is_empty())
        .unwrap_or(MODEL);
    let endpoint = endpoint.trim();
    if !endpoint.is_empty() {
        let url = reqwest::Url::parse(endpoint).ok()?;
        // A key goes only over TLS, or to this computer.
        let safe = url.scheme() == "https"
            || (url.scheme() == "http" && url.host_str().is_some_and(loopback));
        if !safe {
            noted("its address is neither https nor this computer");
            return None;
        }
        let model = if url.host_str() == Some("openrouter.ai") {
            openrouter_model(model)
        } else {
            model.to_owned()
        };
        return Some(Route {
            url: endpoint.to_owned(),
            key: key.unwrap_or_default().to_owned(),
            model,
        });
    }
    if let Some(key) = key {
        return Some(Route {
            url: TYPESAFE.into(),
            key: key.to_owned(),
            model: model.to_owned(),
        });
    }
    let token = account.filter(|token| !token.is_empty())?;
    Some(Route {
        url: format!("{}/systemone", crate::auth::hosted_base()),
        key: token.to_owned(),
        model: model.to_owned(),
    })
}

/// Whether the chat over `endpoint` goes to a model off this computer and
/// its network, so a search's words sent to Jev go nowhere new.
fn hosted(endpoint: &str) -> bool {
    reqwest::Url::parse(endpoint.trim()).is_ok_and(|url| !crate::web::local(&url))
}

/// Whether `host` is this computer.
fn loopback(host: &str) -> bool {
    host == "localhost"
        || host
            .trim_matches(['[', ']'])
            .parse::<std::net::IpAddr>()
            .is_ok_and(|ip| ip.is_loopback())
}

/// OpenRouter's name for a Jev `model`: its moving alias for the latest,
/// TypeSafe's namespace for a pinned version, and a full slug as it is.
fn openrouter_model(model: &str) -> String {
    match model {
        _ if model.contains('/') => model.to_owned(),
        MODEL => "~typesafe/jev-latest".into(),
        version => format!("typesafe/{version}"),
    }
}

/// The question put to Jev: which of `tools` does what `query` asks, or
/// none, and whether it needs a tool at all.
fn request(model: &str, query: &str, tools: &[(&str, &str)]) -> Value {
    let mut criteria = Map::new();
    for (name, description) in tools.iter().take(MOST_OPTIONS - 1) {
        criteria.insert((*name).to_owned(), short(description).into());
    }
    criteria.insert(NONE.into(), "No listed tool does it.".into());
    json!({
        "model": model,
        "state": {"request": query},
        "questions": {
            "tool": {
                "type": "choice",
                "instructions": "Which tool does what the `request` asks? Judge by what each tool does, not by shared words. Pick `none` when no listed tool does it.",
                "criteria": criteria,
            },
            "needs_tool": {
                "type": "noul",
                "instructions": "Does doing what the `request` asks take a tool: an action, or information that must be fetched?",
            },
        },
    })
}

/// `description` on one line, cut at `DESCRIPTION_CHARS`.
fn short(description: &str) -> String {
    let line = description.split_whitespace().collect::<Vec<_>>().join(" ");
    match line.char_indices().nth(DESCRIPTION_CHARS) {
        Some((at, _)) => format!("{}…", &line[..at]),
        None => line,
    }
}

/// Puts `body` to Jev over `route`, giving its answer or what went wrong.
fn ask(route: &Route, body: &Value) -> Result<Value, &'static str> {
    let client = client().ok_or("no HTTP client")?;
    let mut call = client.post(&route.url).json(body);
    if !route.key.is_empty() {
        call = call.bearer_auth(&route.key);
    }
    let response = call.send().map_err(|e| {
        if e.is_timeout() {
            "it did not answer in time"
        } else {
            "it could not be reached"
        }
    })?;
    if !response.status().is_success() {
        return Err("it refused the request");
    }
    response.json().map_err(|_| "its answer was not JSON")
}

/// The client Jev is asked with, which gives up at `DEADLINE`.
fn client() -> Option<&'static reqwest::blocking::Client> {
    static CLIENT: OnceLock<Option<reqwest::blocking::Client>> = OnceLock::new();
    CLIENT
        .get_or_init(|| {
            reqwest::blocking::Client::builder()
                .timeout(DEADLINE)
                .connect_timeout(DEADLINE)
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .ok()
        })
        .as_ref()
}

/// What Jev's `answer` says, or why it says nothing that can be used.
fn read(answer: &Value) -> Result<Judged, &'static str> {
    let choice = &answer["answers"]["tool"];
    let chances = choice["probabilities"]
        .as_object()
        .ok_or("its answer had no chances")?;
    let needed = answer["answers"]["needs_tool"]["noul"].as_f64();
    if choice["choice"] == NONE || needed.is_some_and(|chance| chance < 0.5) {
        return Ok(Judged::Nothing);
    }
    let mut ranked: Vec<(&String, f64)> = chances
        .iter()
        .filter(|(name, _)| *name != NONE)
        .filter_map(|(name, chance)| Some((name, chance.as_f64()?)))
        .filter(|(_, chance)| *chance >= LEAST_CHANCE)
        .collect();
    // Ties go by name, so the same answer ranks the same way.
    ranked.sort_by(|a, b| b.1.total_cmp(&a.1).then_with(|| a.0.cmp(b.0)));
    if ranked.is_empty() {
        return Ok(Judged::Nothing);
    }
    Ok(Judged::Ranked(
        ranked.into_iter().map(|(name, _)| name.clone()).collect(),
    ))
}

/// The answers kept, by what was asked and where.
fn answers() -> &'static Mutex<HashMap<u64, Judged>> {
    static ANSWERS: OnceLock<Mutex<HashMap<u64, Judged>>> = OnceLock::new();
    ANSWERS.get_or_init(Mutex::default)
}

/// What an answer to `body` over `route` is kept under.
fn remembered_as(route: &Route, body: &Value) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    route.url.hash(&mut hasher);
    body.to_string().hash(&mut hasher);
    hasher.finish()
}

/// Notes in the diagnostics log, once for each kind, why Jev was not used.
fn noted(why: &'static str) {
    static NOTED: OnceLock<Mutex<HashSet<&'static str>>> = OnceLock::new();
    let first = NOTED
        .get_or_init(Mutex::default)
        .lock()
        .is_ok_and(|mut noted| noted.insert(why));
    if first {
        crate::diag::event("jev_skipped", &[("why", why.into())]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn on(endpoint: &str) -> Connection {
        Connection {
            endpoint: endpoint.into(),
            ..Connection::default()
        }
    }

    const HOSTED: &str = "https://openrouter.ai/api/v1";
    const LOCAL: &str = "http://127.0.0.1:11434/v1";

    #[test]
    fn auto_asks_through_the_account_only_for_a_hosted_chat() {
        let route = route_of(&on(HOSTED), "", "", "", None, Some("tok")).unwrap();
        assert_eq!(
            route.url,
            format!("{}/systemone", crate::auth::hosted_base())
        );
        assert_eq!((route.key.as_str(), route.model.as_str()), ("tok", MODEL));
        // A model on this computer keeps the search's words on it.
        assert_eq!(
            route_of(&on(LOCAL), "auto", "", "", Some("k"), Some("tok")),
            None
        );
        // Signed out with no key of its own, there is nowhere to ask.
        assert_eq!(route_of(&on(HOSTED), "auto", "", "", None, None), None);
    }

    #[test]
    fn the_settings_choose_where_jev_is_asked() {
        // A key of the user's own goes to TypeSafe, ahead of the account.
        let own = route_of(&on(HOSTED), "", "", "", Some("ts-key"), Some("tok")).unwrap();
        assert_eq!((own.url.as_str(), own.key.as_str()), (TYPESAFE, "ts-key"));
        // An address of their own, OpenRouter's taking its model names.
        let or = "https://openrouter.ai/api/alpha/decisions";
        let routed = route_of(&on(HOSTED), "", or, "", Some("or-key"), None).unwrap();
        assert_eq!(routed.model, "~typesafe/jev-latest");
        let pinned = route_of(&on(HOSTED), "", or, "jev-1.13", Some("or-key"), None).unwrap();
        assert_eq!(pinned.model, "typesafe/jev-1.13");
        // "jev" asks even for a local chat; "names" never does.
        assert!(route_of(&on(LOCAL), "jev", "", "", None, Some("tok")).is_some());
        assert_eq!(
            route_of(&on(HOSTED), "names", "", "", Some("k"), Some("tok")),
            None
        );
        // No key goes out in the clear to another machine.
        let clear = "http://jev.example/v1/systemone";
        assert_eq!(route_of(&on(HOSTED), "", clear, "", Some("k"), None), None);
        let here = "http://127.0.0.1:9/v1/systemone";
        assert!(route_of(&on(HOSTED), "", here, "", Some("k"), None).is_some());
    }

    #[test]
    fn the_question_sends_the_search_and_the_tools_and_nothing_else() {
        let long = "word ".repeat(100);
        let body = request(MODEL, "look at a website", &[("browse", &long)]);
        assert_eq!(body["state"], json!({"request": "look at a website"}));
        let criteria = body["questions"]["tool"]["criteria"].as_object().unwrap();
        assert_eq!(
            criteria.keys().collect::<Vec<_>>(),
            ["browse", NONE].iter().collect::<Vec<_>>()
        );
        assert!(criteria["browse"].as_str().unwrap().chars().count() <= DESCRIPTION_CHARS + 1);
        assert_eq!(body["questions"]["needs_tool"]["type"], "noul");
        // One question holds 255 options at most, none among them.
        let names: Vec<String> = (0..300).map(|i| format!("t{i}")).collect();
        let many: Vec<(&str, &str)> = names.iter().map(|n| (n.as_str(), "")).collect();
        let body = request(MODEL, "x", &many);
        let criteria = body["questions"]["tool"]["criteria"].as_object().unwrap();
        assert_eq!(criteria.len(), MOST_OPTIONS);
    }

    #[test]
    fn an_answer_ranks_by_chance_and_can_say_no_tool_does_it() {
        let answer = |choice: &str, needs: f64| {
            json!({"answers": {
                "tool": {"type": "choice", "choice": choice, "confidence": 0.8,
                    "probabilities": {"browse": 0.7, "use_page": 0.26, "speak": 0.01, "none": 0.03}},
                "needs_tool": {"type": "noul", "noul": needs},
            }})
        };
        assert_eq!(
            read(&answer("browse", 0.9)),
            Ok(Judged::Ranked(vec!["browse".into(), "use_page".into()]))
        );
        assert_eq!(read(&answer("none", 0.9)), Ok(Judged::Nothing));
        assert_eq!(read(&answer("browse", 0.2)), Ok(Judged::Nothing));
        assert!(read(&json!({"error": "no"})).is_err());
    }
}
