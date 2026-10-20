//! What Letronna knows about a model: how much it can read in one request,
//! how much it can write in one reply, what it takes in, which parameters
//! it accepts, and how hard it can be told to think.
//!
//! Three sources, cheapest first. The gateways' model lists already say most
//! of it -- OpenRouter gives the window, the output limit, the kinds of input
//! and the parameters, and the hosted gateway all but the parameters -- so
//! listing models keeps that instead of only the ids. Where a list says
//! nothing, as OpenAI's does, a small table of well-known families fills in
//! the window and the output limit by the longest prefix of the id. And a
//! provider's error can give away a limit lower than either, which is the one
//! the route actually enforces; what is learned that way only ever lowers
//! what is known.
//!
//! Facts are kept per host and model, because one id served somewhere else
//! is another route: Ollama serves a smaller window than a model was trained
//! for, and a proxy may cap the output. They live in memory only and are put
//! together when asked, from what is already here. Nothing is fetched to
//! answer, so asking never holds up a request. Instead the first request to a
//! host whose list is not known asks for it on a thread of its own, so the
//! terminal, helpers and a window whose own listing failed learn the same as
//! a window that listed its gateways, from the next request on. Only a
//! request that sets an effort waits for that list, a few seconds at most,
//! because the level it sends is one the list says the model offers.

use serde_json::Value;
use std::{
    collections::{HashMap, HashSet},
    sync::{Mutex, OnceLock, mpsc},
    time::{Duration, Instant},
};

/// What is known about one model on one host; `None` is not known.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Facts {
    /// The most tokens one request may hold, prompt and reply together.
    pub context: Option<u64>,
    /// The most tokens one reply may run to.
    pub max_output: Option<u64>,
    /// The kinds of input it takes, such as `text` and `image`.
    pub inputs: Option<Vec<String>>,
    /// The request parameters its route accepts, such as `tools` and `reasoning`.
    pub parameters: Option<Vec<String>>,
    /// The effort levels it offers, least first; an empty list offers none.
    pub efforts: Option<Vec<Effort>>,
    /// What it costs, as its gateway's list gives it.
    pub price: Option<Price>,
    /// Who made it, such as `Anthropic`, as the hosted gateway's list says in
    /// `owned_by`.
    pub maker: Option<String>,
}

/// What a model costs, in US dollars a token.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Price {
    pub input: f64,
    pub output: f64,
    /// A token served from the prompt cache, where the list says.
    pub cache_read: Option<f64>,
    /// A token written to the prompt cache, where the list says.
    pub cache_write: Option<f64>,
}

/// One effort level a model offers.
#[derive(Clone, Debug, PartialEq)]
pub struct Effort {
    /// What `reasoning_effort` is set to, such as `xhigh`.
    pub id: String,
    /// What it is called, such as `Extra high`.
    pub label: String,
}

/// A limit a provider's error gave away.
#[derive(Clone, Copy, Debug)]
pub enum Limit {
    /// The most tokens one request may hold.
    Context(u64),
    /// The most tokens one reply may run to.
    Output(u64),
}

/// Windows and output limits, in tokens, of hosted families whose gateways
/// may not list them, as their makers published them by mid-2026. Open-weight
/// families are left out on purpose: the same names run on Ollama, which
/// serves a far smaller window than the one they were trained for. A member
/// whose limits are lower than its family's needs a row of its own: a guess
/// that is too high fails every request until an error teaches the real one.
const FAMILIES: &[(&str, u64, u64)] = &[
    ("gpt-4o", 128_000, 16_384),
    ("gpt-4o-2024-05-13", 128_000, 4_096),
    ("gpt-4.1", 1_047_576, 32_768),
    ("gpt-5", 400_000, 128_000),
    ("gpt-5-chat", 128_000, 16_384),
    ("gpt-5.1-chat", 128_000, 16_384),
    ("gpt-5.2-chat", 128_000, 16_384),
    ("o1", 200_000, 100_000),
    ("o1-mini", 128_000, 65_536),
    ("o1-preview", 128_000, 32_768),
    ("o3", 200_000, 100_000),
    ("o4-mini", 200_000, 100_000),
    ("claude-fable-5", 1_000_000, 128_000),
    ("claude-mythos-5", 1_000_000, 128_000),
    ("claude-opus-5", 1_000_000, 128_000),
    ("claude-sonnet-5", 1_000_000, 128_000),
    ("claude-opus-4-8", 1_000_000, 128_000),
    ("claude-opus-4-7", 1_000_000, 128_000),
    ("claude-opus-4-6", 1_000_000, 128_000),
    ("claude-opus-4-5", 200_000, 64_000),
    ("claude-opus-4", 200_000, 32_000),
    ("claude-sonnet-4-6", 1_000_000, 128_000),
    ("claude-sonnet-4", 200_000, 64_000),
    ("claude-haiku-4-5", 200_000, 64_000),
    ("gemini-3", 1_048_576, 65_536),
    ("gemini-2.5", 1_048_576, 65_536),
    ("gemini-2.0", 1_048_576, 8_192),
    ("deepseek-chat", 128_000, 8_192),
    ("deepseek-reasoner", 128_000, 65_536),
];

/// Effort levels from least to most, by the names gateways give them. A
/// saved level a model does not offer goes to the nearest one it does by
/// this order; `ultra` is past `max` because the hosted list has it there.
const EFFORT_ORDER: &[&str] = &[
    "none", "minimal", "low", "medium", "high", "xhigh", "max", "ultra",
];

/// The levels offered when a model's list does not say: the three Letronna
/// always offered, so such a model is sent what it was sent before.
const USUAL_EFFORTS: [(&str, &str); 3] = [("low", "Low"), ("medium", "Medium"), ("high", "High")];

/// Anthropic's effort levels, least first, with what they are called: its
/// list says which of them a model offers, but not what to call them.
const ANTHROPIC_EFFORTS: [(&str, &str); 5] = [
    ("low", "Low"),
    ("medium", "Medium"),
    ("high", "High"),
    ("xhigh", "Extra high"),
    ("max", "Max"),
];

/// How long a host whose list did not come, or came empty, is left before it
/// is asked again: ten minutes keeps a gateway without a list from being asked
/// on every round of a turn, and one that was only down for a moment is not
/// long without it.
const ASK_AGAIN_AFTER: Duration = Duration::from_secs(10 * 60);

/// How long a request that sets an effort waits for a list it has just asked
/// for: the hosted list comes in well under a second, and a gateway slower
/// than this should not hold up the answer, which then goes with the levels
/// a list that says nothing would give.
const WAIT_FOR_LIST: Duration = Duration::from_secs(5);

/// Everything learned so far, by (host, model).
#[derive(Default)]
struct Known {
    /// What each gateway's model list said.
    listed: HashMap<(String, String), Facts>,
    /// Limits from providers' errors; only `context` and `max_output` are set.
    learned: HashMap<(String, String), Facts>,
    /// The hosts whose list has been read.
    lists: HashSet<String>,
    /// When each host was last asked for its list by `ask_for_list`.
    asked: HashMap<String, Instant>,
}

fn known() -> &'static Mutex<Known> {
    static KNOWN: OnceLock<Mutex<Known>> = OnceLock::new();
    KNOWN.get_or_init(Default::default)
}

/// Where the window `facts_or` gives a model comes from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    /// Its gateway's list, under the id it was asked for by.
    Listed,
    /// Its gateway's list, under an id that differs only by its vendor's path.
    ListedAlias,
    /// `FAMILIES`.
    Family,
    /// The window the settings or `LETRONNA_CONTEXT_WINDOW` assume.
    Setting,
    /// A provider's error, lower than any of those.
    Learned,
}

impl Source {
    /// What it is called where a run reports it.
    pub fn name(self) -> &'static str {
        match self {
            Source::Listed => "listed",
            Source::ListedAlias => "listed-alias",
            Source::Family => "family",
            Source::Setting => "setting",
            Source::Learned => "learned",
        }
    }
}

/// What is known about `model` at `endpoint`, put together from memory.
pub fn facts(endpoint: &str, model: &str) -> Facts {
    facts_or(endpoint, model, None, None)
}

/// What is known about `model` at `endpoint`, with `window` and `output`
/// standing in for limits neither its list nor its family gives, which a
/// limit learned from an error still lowers.
pub(crate) fn facts_or(
    endpoint: &str,
    model: &str,
    window: Option<u64>,
    output: Option<u64>,
) -> Facts {
    put_together(endpoint, model, window, output).0
}

/// The window `facts_or` gives `model` at `endpoint` with `window` standing
/// in, and where it comes from; None when nothing gives one.
pub(crate) fn window_or(endpoint: &str, model: &str, window: Option<u64>) -> Option<(u64, Source)> {
    let (facts, source) = put_together(endpoint, model, window, None);
    facts.context.zip(source)
}

/// `facts_or`, with where its window comes from.
fn put_together(
    endpoint: &str,
    model: &str,
    window: Option<u64>,
    output: Option<u64>,
) -> (Facts, Option<Source>) {
    let host = host(endpoint);
    let key = (host.clone(), model.to_owned());
    let (listed, alias, learned) = known()
        .lock()
        .map(|known| {
            let listed = known.listed.get(&key).cloned();
            // Only when the list has no entry under this id, so one it has
            // keeps what it says even where that is nothing.
            let alias = match listed {
                Some(_) => Facts::default(),
                None => listed_alias(&known, &host, model),
            };
            (
                listed.unwrap_or_default(),
                alias,
                known.learned.get(&key).cloned().unwrap_or_default(),
            )
        })
        .unwrap_or_default();
    let family = family(model);
    let given = [
        (listed.context, Source::Listed),
        (alias.context, Source::ListedAlias),
        (family.context, Source::Family),
        (window, Source::Setting),
    ]
    .into_iter()
    .find_map(|(context, source)| Some((context?, source)));
    let context = match (given, learned.context) {
        (Some((given, _)), Some(learned)) if learned < given => Some((learned, Source::Learned)),
        (None, Some(learned)) => Some((learned, Source::Learned)),
        (given, _) => given,
    };
    let facts = Facts {
        context: context.map(|(tokens, _)| tokens),
        max_output: lowest(
            (listed.max_output)
                .or(alias.max_output)
                .or(family.max_output)
                .or(output),
            learned.max_output,
        ),
        inputs: listed.inputs,
        parameters: listed.parameters,
        efforts: listed.efforts,
        price: listed.price,
        maker: listed.maker,
    };
    (facts, context.map(|(_, source)| source))
}

/// The window and output limit `host`'s list gives under ids that name
/// `model` with another vendor's path, as a router lists one model under
/// each route it reaches it by: the lowest of each, since those routes may
/// serve it differently. Nothing else is taken, because which parameters and
/// levels a route accepts, and what it charges, are that route's own.
fn listed_alias(known: &Known, host: &str, model: &str) -> Facts {
    let name = plain(model);
    let mut alias = Facts::default();
    for ((listed_host, id), facts) in &known.listed {
        if listed_host == host && plain(id) == name {
            alias.context = lowest(alias.context, facts.context);
            alias.max_output = lowest(alias.max_output, facts.max_output);
        }
    }
    alias
}

/// The effort levels `model` at `endpoint` offers, least first: those its
/// gateway's list gives, none when the list says it cannot reason, or low,
/// medium and high when the list says nothing.
pub fn efforts(endpoint: &str, model: &str) -> Vec<Effort> {
    let facts = facts(endpoint, model.trim());
    if let Some(levels) = facts.efforts {
        return levels;
    }
    // OpenRouter says a route cannot reason by leaving reasoning out of the
    // parameters it takes.
    let reasons = |taken: Vec<String>| taken.iter().any(|name| name == "reasoning");
    if facts.parameters.is_some_and(|taken| !reasons(taken)) {
        return Vec::new();
    }
    USUAL_EFFORTS
        .iter()
        .map(|&(id, label)| Effort {
            id: id.to_owned(),
            label: label.to_owned(),
        })
        .collect()
}

/// The level of `levels` to send for the saved `effort`: that one when it is
/// offered, otherwise the nearest by `EFFORT_ORDER`, and `None` for automatic,
/// a level not in that order, or no levels at all.
pub fn clamp<'a>(levels: &'a [Effort], effort: &str) -> Option<&'a Effort> {
    if let Some(level) = levels.iter().find(|level| level.id == effort) {
        return Some(level);
    }
    let rank = |id: &str| EFFORT_ORDER.iter().position(|known| *known == id);
    let wanted = rank(effort)?;
    levels
        .iter()
        .filter_map(|level| Some((rank(&level.id)?, level)))
        // Between two as near, the higher: an answer thought through a little
        // more costs a little more, one thought through too little is worse.
        .min_by_key(|&(at, _)| (at.abs_diff(wanted), std::cmp::Reverse(at)))
        .map(|(_, level)| level)
}

/// Remember a limit a provider's error gave away for `model` at `endpoint`.
pub fn learn(endpoint: &str, model: &str, limit: Limit) {
    let Ok(mut known) = known().lock() else {
        return;
    };
    let learned = known
        .learned
        .entry((host(endpoint), model.to_owned()))
        .or_default();
    let (slot, tokens) = match limit {
        Limit::Context(tokens) => (&mut learned.context, tokens),
        Limit::Output(tokens) => (&mut learned.max_output, tokens),
    };
    // No provider limits a request to nothing, so a zero is a misread error,
    // and a window of nothing would fail every request after it.
    if tokens == 0 {
        return;
    }
    *slot = lowest(*slot, Some(tokens));
}

/// Ask the gateway a request with a key goes to for its `/models` list on a
/// thread of its own, the first time for its host, and keep what it says;
/// what it gives back hears when that ask is over, and there is none if no ask
/// was made.
pub fn ask_for_list(endpoint: &str, key: &str) -> Option<mpsc::Receiver<()>> {
    // A server that takes no key is one of the user's own, such as Ollama or
    // LM Studio, whose list names its models and says nothing about them.
    if key.trim().is_empty() || !due(&host(endpoint)) {
        return None;
    }
    // A request to a Messages route goes as it is: `list_models` asks such an
    // endpoint at its API's root, with the key as Messages takes it.
    let base = match crate::providers::wire::anthropic::speaks(endpoint) {
        true => endpoint,
        false => endpoint
            .strip_suffix("/chat/completions")
            .unwrap_or(endpoint),
    }
    .to_owned();
    let key = key.to_owned();
    let (over, heard) = mpsc::channel();
    // A list that did not come is not asked for again until `ASK_AGAIN_AFTER`
    // has passed since this ask, so what went wrong does not matter here.
    std::thread::spawn(move || {
        let _ = crate::chat::list_models(&base, &key);
        let _ = over.send(());
    });
    Some(heard)
}

/// Wait for an ask `ask_for_list` made to be over, for up to `WAIT_FOR_LIST`.
pub fn wait_for_list(asked: mpsc::Receiver<()>) {
    let _ = asked.recv_timeout(WAIT_FOR_LIST);
}

/// Whether `host` should be asked for its list now, noting the ask if so.
fn due(host: &str) -> bool {
    let Ok(mut known) = known().lock() else {
        return false;
    };
    let lately = |at: &Instant| at.elapsed() < ASK_AGAIN_AFTER;
    if known.lists.contains(host) || known.asked.get(host).is_some_and(lately) {
        return false;
    }
    known.asked.insert(host.to_owned(), Instant::now());
    true
}

/// Keep what a gateway's `/models` list says about each of its models.
pub(crate) fn remember(endpoint: &str, list: &[Value]) {
    let host = host(endpoint);
    let Ok(mut known) = known().lock() else {
        return;
    };
    // An empty list may be a moment's hiccup, so it is asked for again later.
    if !list.is_empty() {
        known.lists.insert(host.clone());
    }
    for entry in list {
        let Some(id) = entry["id"].as_str().or_else(|| entry["key"].as_str()) else {
            continue;
        };
        known
            .listed
            .insert((host.clone(), id.to_owned()), read(entry));
    }
}

/// The facts in one entry of a `/models` list, in OpenRouter's shape, one
/// that keeps them at the top, as the hosted gateway, vLLM and Groq do, or
/// Anthropic's; OpenAI's entries hold none.
fn read(entry: &Value) -> Facts {
    let tokens = |value: &Value| value.as_u64().filter(|&n| n > 0);
    let names = |value: &Value| {
        value.as_array().map(|list| {
            list.iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect()
        })
    };
    let capabilities = &entry["capabilities"];
    Facts {
        // vLLM and SGLang call the window they serve `max_model_len`, and Groq
        // calls it `context_window`: the window that route holds a request
        // to, not the one the model was trained for. Anthropic's calls it
        // `max_input_tokens`.
        context: tokens(&entry["context_length"])
            .or_else(|| tokens(&entry["top_provider"]["context_length"]))
            .or_else(|| tokens(&entry["max_model_len"]))
            .or_else(|| tokens(&entry["context_window"]))
            .or_else(|| tokens(&entry["max_input_tokens"])),
        // A `max_tokens` that is really the window does no harm: the reply
        // asked for is a quarter of a known window at most, and no more than
        // usual when the window is not known.
        max_output: tokens(&entry["top_provider"]["max_completion_tokens"])
            .or_else(|| tokens(&entry["max_output_tokens"]))
            .or_else(|| tokens(&entry["max_completion_tokens"]))
            .or_else(|| tokens(&entry["max_tokens"])),
        inputs: names(&entry["architecture"]["input_modalities"])
            .or_else(|| names(&entry["input_modalities"]))
            .or_else(|| sights(capabilities)),
        parameters: names(&entry["supported_parameters"]),
        // The hosted gateway leaves the levels out for a model that cannot
        // reason rather than listing none, and says so in its capabilities.
        efforts: entry["reasoning_efforts"]
            .as_array()
            .map(|list| {
                list.iter()
                    .filter_map(|level| {
                        let id = level["effort"].as_str()?;
                        Some(Effort {
                            id: id.to_owned(),
                            label: level["label"].as_str().unwrap_or(id).to_owned(),
                        })
                    })
                    .collect()
            })
            .or_else(|| {
                let reasons = capabilities["reasoning"].as_bool();
                (reasons == Some(false)).then(Vec::new)
            })
            .or_else(|| levels(&capabilities["effort"])),
        price: price(&entry["pricing"]),
        maker: maker(entry),
    }
}

/// Who made the model in a list entry: its `owned_by`, or the `maker` the
/// hosted catalogue calls it. OpenAI's list says `system` and Ollama's
/// `library`, which are no maker.
fn maker(entry: &Value) -> Option<String> {
    let named = (entry["owned_by"].as_str()).or_else(|| entry["maker"].as_str())?;
    let named = named.trim();
    (!named.is_empty() && !matches!(named, "system" | "library")).then(|| named.to_owned())
}

/// What Anthropic's `capabilities` say a model takes in: text, and pictures
/// when it can see them.
fn sights(capabilities: &Value) -> Option<Vec<String>> {
    let sees = capabilities["image_input"]["supported"].as_bool()?;
    let inputs = if sees {
        &["text", "image"][..]
    } else {
        &["text"]
    };
    Some(inputs.iter().map(|input| (*input).to_owned()).collect())
}

/// The effort levels Anthropic's `capabilities` say a model offers, least
/// first; none when it offers no effort.
fn levels(effort: &Value) -> Option<Vec<Effort>> {
    effort["supported"].as_bool()?;
    let offered = ANTHROPIC_EFFORTS
        .iter()
        .filter(|(id, _)| effort[*id]["supported"] == true)
        .map(|&(id, label)| Effort {
            id: id.to_owned(),
            label: label.to_owned(),
        });
    Some(offered.collect())
}

/// The price in a list entry's `pricing`, in OpenRouter's shape: dollars a
/// token, written as strings. Its router lists -1 for a price it knows only
/// once it has picked a model, which is no price at all.
fn price(pricing: &Value) -> Option<Price> {
    let rate = |key: &str| {
        let value = &pricing[key];
        value
            .as_str()
            .and_then(|text| text.trim().parse::<f64>().ok())
            .or_else(|| value.as_f64())
            .filter(|rate| rate.is_finite() && *rate >= 0.)
    };
    Some(Price {
        input: rate("prompt")?,
        output: rate("completion")?,
        cache_read: rate("input_cache_read"),
        cache_write: rate("input_cache_write"),
    })
}

/// The window and output limit of the longest family in `FAMILIES` that
/// `model` starts with, up to a whole part of the id.
fn family(model: &str) -> Facts {
    let id = plain(model);
    FAMILIES
        .iter()
        .filter(|(prefix, ..)| {
            // Dots read as dashes, so `gpt-4-1106` starts with `gpt-4.1` unless a
            // family has to end where a part of the id does.
            id.strip_prefix(&plain(prefix))
                .is_some_and(|rest| !rest.starts_with(|c: char| c.is_ascii_alphanumeric()))
        })
        .max_by_key(|(prefix, ..)| prefix.len())
        .map(|&(_, context, output)| Facts {
            context: Some(context),
            max_output: Some(output),
            ..Facts::default()
        })
        .unwrap_or_default()
}

/// A model id without its vendor's path, in lower case and with dots as
/// dashes: OpenRouter's `anthropic/claude-opus-4.6` is Anthropic's
/// `claude-opus-4-6`.
fn plain(id: &str) -> String {
    let name = id.rsplit('/').next().unwrap_or(id);
    name.to_lowercase().replace('.', "-")
}

/// The host and port an endpoint points at: two servers on one machine are
/// two routes, while one server under two paths is one.
fn host(endpoint: &str) -> String {
    let endpoint = endpoint.trim();
    let Ok(url) = reqwest::Url::parse(endpoint) else {
        return endpoint.to_owned();
    };
    match (url.host_str(), url.port_or_known_default()) {
        (Some(host), Some(port)) => format!("{host}:{port}"),
        _ => endpoint.to_owned(),
    }
}

/// The lower of two limits, or whichever one is known.
fn lowest(a: Option<u64>, b: Option<u64>) -> Option<u64> {
    a.into_iter().chain(b).min()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chat::{Connection, Event, Message};
    use serde_json::json;
    use std::{
        io::{Read, Write},
        net::{TcpListener, TcpStream},
        sync::Arc,
    };

    #[test]
    fn an_openrouter_list_keeps_the_window_the_output_limit_and_what_it_takes() {
        let list = json!({"data": [
            {
                "id": "anthropic/claude-sonnet-4.5",
                "name": "Anthropic: Claude Sonnet 4.5",
                "context_length": 1_000_000,
                "architecture": {"modality": "text+image->text", "input_modalities": ["text", "image", "file"], "output_modalities": ["text"]},
                "pricing": {"prompt": "0.000003", "completion": "0.000015"},
                "top_provider": {"context_length": 1_000_000, "max_completion_tokens": 32_000, "is_moderated": true},
                "supported_parameters": ["max_tokens", "reasoning", "tool_choice", "tools"]
            },
            {
                "id": "some-lab/new-model",
                "context_length": null,
                "architecture": {"input_modalities": ["text"]},
                "top_provider": {"context_length": 32_768, "max_completion_tokens": null},
                "supported_parameters": []
            }
        ]});
        let endpoint = "https://openrouter.test/api/v1";
        remember(endpoint, list["data"].as_array().unwrap());

        let sonnet = facts(endpoint, "anthropic/claude-sonnet-4.5");
        assert_eq!(sonnet.context, Some(1_000_000));
        // Lower than the family's 64,000, so this can only come from the list.
        assert_eq!(sonnet.max_output, Some(32_000));
        assert_eq!(
            sonnet.inputs.as_deref(),
            Some(&["text".to_owned(), "image".into(), "file".into()][..])
        );
        assert!(sonnet.parameters.unwrap().contains(&"reasoning".to_owned()));

        let new = facts(endpoint, "some-lab/new-model");
        assert_eq!(new.context, Some(32_768));
        assert_eq!(new.max_output, None);
        assert_eq!(new.parameters, Some(vec![]));
    }

    #[test]
    fn an_anthropic_list_gives_the_window_the_output_cap_pictures_and_efforts() {
        let list = json!({"data": [
            {
                "type": "model",
                "id": "claude-opus-5-5",
                "display_name": "Claude Opus 5.5",
                "max_input_tokens": 1_000_000,
                "max_tokens": 128_000,
                "capabilities": {
                    "image_input": {"supported": true},
                    "thinking": {"supported": true, "types": {"adaptive": {"supported": true}}},
                    "effort": {"supported": true, "low": {"supported": true},
                        "medium": {"supported": true}, "high": {"supported": true},
                        "xhigh": {"supported": true}, "max": {"supported": true}}
                }
            },
            {
                "type": "model",
                "id": "claude-haiku-4-5",
                "max_input_tokens": 200_000,
                "max_tokens": 32_000,
                "capabilities": {
                    "image_input": {"supported": false},
                    "effort": {"supported": false, "low": {"supported": false}}
                }
            }
        ]});
        let endpoint = "https://anthropic.test/v1";
        remember(endpoint, list["data"].as_array().unwrap());

        let opus = facts(endpoint, "claude-opus-5-5");
        assert_eq!(
            (opus.context, opus.max_output),
            (Some(1_000_000), Some(128_000))
        );
        assert_eq!(opus.inputs, Some(vec!["text".to_owned(), "image".into()]));
        let levels: Vec<String> = (efforts(endpoint, "claude-opus-5-5").into_iter())
            .map(|level| level.id)
            .collect();
        assert_eq!(levels, ["low", "medium", "high", "xhigh", "max"]);

        let haiku = facts(endpoint, "claude-haiku-4-5");
        // Lower than the family's 64,000, so this can only come from the list.
        assert_eq!(haiku.max_output, Some(32_000));
        assert_eq!(haiku.inputs, Some(vec!["text".to_owned()]));
        assert!(efforts(endpoint, "claude-haiku-4-5").is_empty());
    }

    #[test]
    fn a_listed_price_is_kept_and_a_price_not_known_yet_is_none() {
        let listed = price(&json!({"prompt": "0.000003", "completion": "0.000015",
            "input_cache_read": "0.0000003", "input_cache_write": "0.00000375"}))
        .unwrap();
        assert_eq!((listed.input, listed.output), (0.000003, 0.000015));
        assert_eq!(listed.cache_read, Some(0.0000003));
        assert_eq!(listed.cache_write, Some(0.00000375));
        // OpenRouter's router, which knows only once it has picked a model.
        assert_eq!(price(&json!({"prompt": "-1", "completion": "-1"})), None);
        assert_eq!(price(&json!({"prompt": "0.000001"})), None);
        assert_eq!(price(&Value::Null), None);
    }

    #[test]
    fn an_openai_list_says_nothing_so_a_known_family_fills_in() {
        let list = json!({"object": "list", "data": [
            {"id": "gpt-4o-2024-08-06", "object": "model", "created": 1_722_814_719, "owned_by": "system"},
            {"id": "text-embedding-3-small", "object": "model", "created": 1_705_948_997, "owned_by": "system"}
        ]});
        let endpoint = "https://openai.test/v1";
        remember(endpoint, list["data"].as_array().unwrap());

        let gpt = facts(endpoint, "gpt-4o-2024-08-06");
        assert_eq!((gpt.context, gpt.max_output), (Some(128_000), Some(16_384)));
        assert_eq!((gpt.inputs, gpt.parameters, gpt.maker), (None, None, None));
        assert_eq!(facts(endpoint, "text-embedding-3-small"), Facts::default());
    }

    #[test]
    fn the_longest_family_wins_whatever_the_vendor_path_or_dots() {
        let limits = |model: &str| {
            let facts = facts("https://nowhere.test/v1", model);
            (facts.context, facts.max_output)
        };
        assert_eq!(limits("gpt-5-mini"), (Some(400_000), Some(128_000)));
        assert_eq!(limits("gpt-5-chat-latest"), (Some(128_000), Some(16_384)));
        assert_eq!(limits("gpt-5.1-chat-latest"), (Some(128_000), Some(16_384)));
        assert_eq!(limits("gpt-4o-2024-05-13"), (Some(128_000), Some(4_096)));
        assert_eq!(limits("o1-mini"), (Some(128_000), Some(65_536)));
        assert_eq!(
            limits("openai/gpt-4.1-mini"),
            (Some(1_047_576), Some(32_768))
        );
        assert_eq!(
            limits("anthropic/claude-opus-4.6"),
            limits("claude-opus-4-6")
        );
        assert_eq!(limits("claude-opus-4-6"), (Some(1_000_000), Some(128_000)));
        assert_eq!(
            limits("claude-opus-4-1-20250805"),
            (Some(200_000), Some(32_000))
        );
        assert_eq!(
            limits("models/gemini-2.5-pro"),
            (Some(1_048_576), Some(65_536))
        );
        assert_eq!(limits("llama3.1:8b"), (None, None));
    }

    #[test]
    fn a_family_ends_where_a_part_of_the_id_does() {
        let limits = |model: &str| {
            let facts = facts("https://nowhere.test/v1", model);
            (facts.context, facts.max_output)
        };
        assert_eq!(limits("gpt-4-1106-preview"), (None, None));
        assert_eq!(limits("gpt-4.1"), (Some(1_047_576), Some(32_768)));
        assert_eq!(limits("openai/gpt-4o:free"), (Some(128_000), Some(16_384)));
    }

    #[test]
    fn a_vllm_entry_gives_its_max_model_len_as_the_window() {
        let list = json!({"object": "list", "data": [{
            "id": "Qwen/Qwen3-32B", "object": "model", "created": 1_760_000_000,
            "owned_by": "vllm", "root": "Qwen/Qwen3-32B", "parent": null,
            "max_model_len": 40_960,
            "permission": [{"id": "modelperm-1", "object": "model_permission"}]
        }]});
        let endpoint = "http://vllm.test:8000/v1";
        remember(endpoint, list["data"].as_array().unwrap());

        let served = facts(endpoint, "Qwen/Qwen3-32B");
        assert_eq!((served.context, served.max_output), (Some(40_960), None));
    }

    #[test]
    fn a_groq_entry_gives_its_context_window_and_max_completion_tokens() {
        let list = json!({"object": "list", "data": [{
            "id": "llama-3.3-70b-versatile", "object": "model", "created": 1_733_447_754,
            "owned_by": "Meta", "active": true, "context_window": 131_072,
            "public_apps": null, "max_completion_tokens": 32_768
        }]});
        let endpoint = "https://groq.test/openai/v1";
        remember(endpoint, list["data"].as_array().unwrap());

        let served = facts(endpoint, "llama-3.3-70b-versatile");
        assert_eq!(
            (served.context, served.max_output),
            (Some(131_072), Some(32_768))
        );
    }

    #[test]
    fn a_top_level_max_tokens_is_read_as_the_output_limit() {
        let list = json!([{"id": "some-model", "context_length": 200_000, "max_tokens": 64_000}]);
        let endpoint = "https://max-tokens.test/v1";
        remember(endpoint, list.as_array().unwrap());
        assert_eq!(facts(endpoint, "some-model").max_output, Some(64_000));
    }

    #[test]
    fn the_hosted_list_gives_the_window_and_the_family_the_output_limit() {
        let list = json!({"object": "list", "data": [
            {"id": "claude-sonnet-5", "object": "model", "owned_by": "Anthropic", "context_length": 500_000}
        ]});
        let endpoint = "https://hosted.test/v1";
        remember(endpoint, list["data"].as_array().unwrap());

        let sonnet = facts(endpoint, "claude-sonnet-5");
        assert_eq!(sonnet.context, Some(500_000));
        assert_eq!(sonnet.max_output, Some(128_000));
        // Who made it, which a perk for one maker's models turns on.
        assert_eq!(sonnet.maker.as_deref(), Some("Anthropic"));
    }

    #[test]
    fn a_learned_limit_only_ever_lowers_what_is_known() {
        let endpoint = "https://learning.test/v1";
        learn(endpoint, "gpt-4o", Limit::Context(100_000));
        learn(endpoint, "gpt-4o", Limit::Context(120_000));
        learn(endpoint, "gpt-4o", Limit::Context(0));
        learn(endpoint, "gpt-4o", Limit::Output(500_000));
        let gpt = facts(endpoint, "gpt-4o");
        assert_eq!(gpt.context, Some(100_000));
        assert_eq!(gpt.max_output, Some(16_384));

        learn(endpoint, "gpt-4o", Limit::Output(4_096));
        assert_eq!(facts(endpoint, "gpt-4o").max_output, Some(4_096));

        learn(endpoint, "my-local-model", Limit::Context(8_192));
        assert_eq!(facts(endpoint, "my-local-model").context, Some(8_192));

        let elsewhere = facts("https://elsewhere.test/v1", "gpt-4o");
        assert_eq!(elsewhere.context, Some(128_000));
    }

    #[test]
    fn a_model_listed_under_another_vendors_path_takes_the_lowest_window() {
        let endpoint = "http://router.test:8080/v1";
        let list = json!([
            {"id": "opencode-go/deepseek-v4.1-flash", "context_length": 1_000_000,
                "max_output_tokens": 384_000,
                "pricing": {"prompt": "0.0000001", "completion": "0.0000002"},
                "reasoning_efforts": [{"effort": "high", "label": "High"}],
                "supported_parameters": ["tools"]},
            {"id": "openrouter/deepseek/deepseek-v4.1-flash", "context_length": 1_048_576,
                "top_provider": {"max_completion_tokens": 65_536}}
        ]);
        remember(endpoint, list.as_array().unwrap());

        let alias = facts(endpoint, "deepseek/deepseek-v4.1-flash");
        assert_eq!(
            (alias.context, alias.max_output),
            (Some(1_000_000), Some(65_536))
        );
        // What a route accepts and charges stays that route's own.
        assert_eq!(
            (alias.price, alias.efforts, alias.parameters, alias.inputs),
            (None, None, None, None)
        );
        assert_eq!(
            window_or(endpoint, "deepseek/deepseek-v4.1-flash", Some(8_000)),
            Some((1_000_000, Source::ListedAlias))
        );
        // Another host's list says nothing about this one's routes.
        assert_eq!(
            facts(
                "http://elsewhere.test:8080/v1",
                "deepseek/deepseek-v4.1-flash"
            )
            .context,
            None
        );
        // Nor does it name a model whose id only starts the same.
        assert_eq!(facts(endpoint, "deepseek/deepseek-v4.1").context, None);
    }

    #[test]
    fn a_model_listed_under_its_own_id_keeps_its_own_entry() {
        let endpoint = "https://exact.test/v1";
        let list = json!([
            {"id": "vendor/some-model", "context_length": 32_000},
            {"id": "other/some-model", "context_length": 16_000, "max_output_tokens": 4_096},
            {"id": "says-nothing"},
            {"id": "other/says-nothing", "context_length": 16_000}
        ]);
        remember(endpoint, list.as_array().unwrap());
        let exact = facts(endpoint, "vendor/some-model");
        assert_eq!((exact.context, exact.max_output), (Some(32_000), None));
        assert_eq!(
            window_or(endpoint, "vendor/some-model", None),
            Some((32_000, Source::Listed))
        );
        // An entry that gives no window is still the model's own word.
        assert_eq!(facts(endpoint, "says-nothing").context, None);
        assert_eq!(window_or(endpoint, "says-nothing", None), None);
    }

    #[test]
    fn a_window_says_where_it_comes_from() {
        let endpoint = "https://sources.test/v1";
        assert_eq!(
            window_or(endpoint, "gpt-4o", Some(50_000)),
            Some((128_000, Source::Family))
        );
        assert_eq!(
            window_or(endpoint, "unlisted-model", Some(50_000)),
            Some((50_000, Source::Setting))
        );
        assert_eq!(window_or(endpoint, "unlisted-model", None), None);
        learn(endpoint, "gpt-4o", Limit::Context(100_000));
        assert_eq!(
            window_or(endpoint, "gpt-4o", None),
            Some((100_000, Source::Learned))
        );
        // A learned limit higher than the set window does not take its place.
        learn(endpoint, "unlisted-model", Limit::Context(60_000));
        assert_eq!(
            window_or(endpoint, "unlisted-model", Some(50_000)),
            Some((50_000, Source::Setting))
        );
        assert_eq!(Source::ListedAlias.name(), "listed-alias");
    }

    #[test]
    fn two_servers_on_one_machine_are_two_hosts_and_a_path_is_not() {
        assert_eq!(
            host("https://openrouter.ai/api/v1"),
            host(" HTTPS://OpenRouter.ai/api/v1/chat/completions ")
        );
        assert_ne!(
            host("http://localhost:11434/v1"),
            host("http://localhost:8080/v1")
        );
    }

    /// A gateway on this machine: chat requests are answered at once, and its
    /// model list with `list` -- or not found, for `null` -- once nothing holds
    /// `gate`.
    struct Stub {
        base: String,
        /// The authorization each list was asked for with, in order.
        asks: Arc<Mutex<Vec<String>>>,
        gate: Arc<Mutex<()>>,
    }

    impl Stub {
        fn new(list: Value) -> Self {
            let server = TcpListener::bind("127.0.0.1:0").unwrap();
            let base = format!("http://{}/v1", server.local_addr().unwrap());
            let (asks, gate) = (Arc::default(), Arc::default());
            let (seen, held) = (Arc::clone(&asks), Arc::clone(&gate));
            std::thread::spawn(move || {
                for socket in server.incoming() {
                    let (list, seen, held) = (list.clone(), Arc::clone(&seen), Arc::clone(&held));
                    std::thread::spawn(move || answer(socket.unwrap(), &list, &seen, &held));
                }
            });
            Self { base, asks, gate }
        }

        fn asks(&self) -> usize {
            self.asks.lock().unwrap().len()
        }
    }

    /// Answers one request to a `Stub`.
    fn answer(mut socket: TcpStream, list: &Value, asks: &Mutex<Vec<String>>, gate: &Mutex<()>) {
        let head = head(&mut socket);
        let (status, kind, body) = if head.starts_with("get /v1/models ") {
            let authorization = head
                .lines()
                .find_map(|l| l.strip_prefix("authorization: "))
                .unwrap_or_default();
            asks.lock().unwrap().push(authorization.to_owned());
            drop(gate.lock());
            match list {
                Value::Null => ("404 Not Found", "application/json", "{}".to_owned()),
                list => ("200 OK", "application/json", list.to_string()),
            }
        } else {
            let reply =
                "data: {\"choices\":[{\"delta\":{\"content\":\"Hi\"}}]}\n\ndata: [DONE]\n\n";
            ("200 OK", "text/event-stream", reply.to_owned())
        };
        write!(socket, "HTTP/1.1 {status}\r\nContent-Type: {kind}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
    }

    /// The head of the request on `socket`, in lower case, once its body is in.
    fn head(socket: &mut TcpStream) -> String {
        let mut bytes = Vec::new();
        let mut block = [0; 65536];
        loop {
            let n = socket.read(&mut block).unwrap();
            assert_ne!(n, 0, "the request ended early");
            bytes.extend_from_slice(&block[..n]);
            let Some(end) = bytes.windows(4).position(|w| w == b"\r\n\r\n") else {
                continue;
            };
            let head = String::from_utf8_lossy(&bytes[..end]).to_lowercase();
            let length = head
                .lines()
                .find_map(|l| l.strip_prefix("content-length: "))
                .map_or(0, |n| n.parse().unwrap());
            if bytes.len() >= end + 4 + length {
                return head;
            }
        }
    }

    /// Sends one chat request for `model` to `base` with `key`, and what came
    /// of it, failing if it takes more than a few seconds.
    fn send(base: &str, model: &str, key: &str) -> Result<(), String> {
        send_with_effort(base, model, key, "")
    }

    /// Like `send`, asking the model to think as hard as `effort`.
    fn send_with_effort(base: &str, model: &str, key: &str, effort: &str) -> Result<(), String> {
        let connection = Connection {
            endpoint: base.into(),
            model: model.into(),
            key: key.into(),
            effort: effort.into(),
            ..Default::default()
        };
        let prompt = Message {
            role: "user".into(),
            content: "Hi".into(),
            ..Default::default()
        };
        let events = crate::chat::start(connection, vec![prompt]);
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            match events.try_recv() {
                Ok(Event::Finished(result)) => return result.map_err(|e| e.to_string()),
                Ok(_) => {}
                Err(_) => std::thread::sleep(Duration::from_millis(5)),
            }
        }
        panic!("the request did not finish");
    }

    /// Waits for `done`, failing if it does not come within a few seconds.
    fn until(what: &str, done: impl Fn() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while !done() {
            assert!(Instant::now() < deadline, "{what} never came");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    /// Long enough for an ask a request started to reach its gateway.
    const SETTLE: Duration = Duration::from_millis(300);

    #[test]
    fn an_unknown_host_is_asked_for_its_list_once_in_the_background() {
        let stub = Stub::new(json!({"object": "list", "data": [
            {"id": "some-model", "object": "model", "context_length": 64_000}
        ]}));
        assert_eq!(facts(&stub.base, "some-model").context, None);
        for _ in 0..3 {
            send(&stub.base, "some-model", "test-key").unwrap();
        }
        until("the list", || {
            facts(&stub.base, "some-model").context == Some(64_000)
        });
        std::thread::sleep(SETTLE);
        assert_eq!(*stub.asks.lock().unwrap(), ["bearer test-key"]);
    }

    #[test]
    fn a_request_leaving_effort_to_the_model_never_waits_for_the_list() {
        let stub = Stub::new(json!({"data": [{"id": "some-model", "context_length": 32_000}]}));
        let hold = stub.gate.lock().unwrap();
        send(&stub.base, "some-model", "test-key").unwrap();
        send(&stub.base, "some-model", "test-key").unwrap();
        until("the ask", || stub.asks() == 1);
        assert_eq!(facts(&stub.base, "some-model").context, None);
        drop(hold);
        until("the list", || {
            facts(&stub.base, "some-model").context == Some(32_000)
        });
    }

    #[test]
    fn a_request_setting_an_effort_waits_for_the_list() {
        let stub = Stub::new(json!({"data": [
            {"id": "some-model", "reasoning_efforts": [{"effort": "high", "label": "High"}]}
        ]}));
        let hold = stub.gate.lock().unwrap();
        let base = stub.base.clone();
        let request =
            std::thread::spawn(move || send_with_effort(&base, "some-model", "test-key", "max"));
        std::thread::sleep(SETTLE);
        assert!(!request.is_finished());
        drop(hold);
        request.join().unwrap().unwrap();
        // No waiting here: the levels were in before the request was sent.
        assert!(facts(&stub.base, "some-model").efforts.is_some());
    }

    #[test]
    fn a_host_already_listed_is_not_asked_again() {
        let stub = Stub::new(json!({"data": [{"id": "some-model", "context_length": 16_000}]}));
        // As the window lists it when it opens.
        crate::chat::list_models(&stub.base, "test-key").unwrap();
        send(&stub.base, "some-model", "test-key").unwrap();
        send(&stub.base, "some-model", "test-key").unwrap();
        std::thread::sleep(SETTLE);
        assert_eq!(stub.asks(), 1);
        assert_eq!(facts(&stub.base, "some-model").context, Some(16_000));
    }

    #[test]
    fn a_request_without_a_key_does_not_ask() {
        let stub = Stub::new(json!({"data": [{"id": "some-model", "context_length": 16_000}]}));
        send(&stub.base, "some-model", "").unwrap();
        std::thread::sleep(SETTLE);
        assert_eq!(stub.asks(), 0);
    }

    #[test]
    fn a_host_whose_list_came_empty_is_asked_again() {
        let stub = Stub::new(json!({"data": []}));
        crate::chat::list_models(&stub.base, "test-key").unwrap();
        send(&stub.base, "some-model", "test-key").unwrap();
        until("the second ask", || stub.asks() == 2);
    }

    #[test]
    fn a_host_without_a_list_is_not_asked_on_every_request() {
        let stub = Stub::new(Value::Null);
        send(&stub.base, "some-model", "test-key").unwrap();
        until("the ask", || stub.asks() == 1);
        // Long enough for that ask to have failed.
        std::thread::sleep(SETTLE);
        send(&stub.base, "some-model", "test-key").unwrap();
        send(&stub.base, "some-model", "test-key").unwrap();
        std::thread::sleep(SETTLE);
        assert_eq!(stub.asks(), 1);
    }

    #[test]
    fn the_hosted_gateways_context_length_is_kept() {
        // Two entries as the hosted gateway sends them: an open-weight model,
        // which no family covers, and a small one. Their capabilities are not
        // the parameters a route accepts, and false can mean not known, so
        // they are left out.
        let stub = Stub::new(json!({"object": "list", "data": [
            {
                "id": "kimi-k2.6", "object": "model", "owned_by": "Moonshot",
                "context_length": 262_144, "max_output_tokens": 65_536,
                "input_modalities": ["text", "image"],
                "capabilities": {"tool_calling": true, "reasoning": true},
                "reasoning_efforts": [{"effort": "high", "label": "High"}],
                "knowledge_cutoff": "2024-10"
            },
            {
                "id": "hy-mt2-7b", "object": "model", "owned_by": "Tencent",
                "context_length": 8_192, "max_output_tokens": 4_096,
                "input_modalities": ["text"],
                "capabilities": {"tool_calling": false, "reasoning": false}
            }
        ]}));
        send(&stub.base, "kimi-k2.6", "test-key").unwrap();
        until("the list", || {
            facts(&stub.base, "kimi-k2.6").context.is_some()
        });

        let open_weight = facts(&stub.base, "kimi-k2.6");
        assert_eq!(
            (open_weight.context, open_weight.max_output),
            (Some(262_144), Some(65_536))
        );
        assert_eq!(
            open_weight.inputs,
            Some(vec!["text".to_owned(), "image".into()])
        );
        assert_eq!(open_weight.parameters, None);

        let small = facts(&stub.base, "hy-mt2-7b");
        assert_eq!(
            (small.context, small.max_output),
            (Some(8_192), Some(4_096))
        );
    }

    /// Remembers four entries of the hosted gateway's list at `endpoint`:
    /// three with the levels it sends for them, and one that cannot reason.
    fn hosted_efforts(endpoint: &str) {
        let list = json!([
            {"id": "claude-opus-5", "reasoning_efforts": [
                {"effort": "low", "label": "Low"}, {"effort": "medium", "label": "Medium"},
                {"effort": "high", "label": "High"}, {"effort": "xhigh", "label": "Extra high"},
                {"effort": "max", "label": "Max"}
            ]},
            {"id": "gpt-5.5", "reasoning_efforts": [
                {"effort": "low", "label": "Light"}, {"effort": "medium", "label": "Medium"},
                {"effort": "high", "label": "High"}, {"effort": "xhigh", "label": "Extra high"}
            ]},
            {"id": "deepseek-v4-flash", "reasoning_efforts": [
                {"effort": "none", "label": "None"}, {"effort": "low", "label": "Low"},
                {"effort": "high", "label": "High"}, {"effort": "max", "label": "Max"}
            ]},
            {"id": "hy-mt2-7b", "capabilities": {"tool_calling": false, "reasoning": false}}
        ]);
        remember(endpoint, list.as_array().unwrap());
    }

    /// The ids of `levels`, in order.
    fn ids(levels: &[Effort]) -> Vec<&str> {
        levels.iter().map(|level| level.id.as_str()).collect()
    }

    #[test]
    fn effort_levels_come_from_the_models_list() {
        let endpoint = "https://efforts.test/v1";
        hosted_efforts(endpoint);
        assert_eq!(
            ids(&efforts(endpoint, "claude-opus-5")),
            ["low", "medium", "high", "xhigh", "max"]
        );
        assert_eq!(
            ids(&efforts(endpoint, "deepseek-v4-flash")),
            ["none", "low", "high", "max"]
        );
        let gpt = efforts(endpoint, "gpt-5.5");
        assert_eq!(ids(&gpt), ["low", "medium", "high", "xhigh"]);
        let labels: Vec<_> = gpt.iter().map(|level| level.label.as_str()).collect();
        assert_eq!(labels, ["Light", "Medium", "High", "Extra high"]);
        assert!(efforts(endpoint, "hy-mt2-7b").is_empty());
        // A level without a label goes by its id.
        let unlabelled = json!([{"id": "unlabelled", "reasoning_efforts": [{"effort": "high"}]}]);
        remember(endpoint, unlabelled.as_array().unwrap());
        assert_eq!(efforts(endpoint, "unlabelled")[0].label, "high");
    }

    #[test]
    fn a_level_the_model_lacks_is_clamped_to_the_nearest() {
        let endpoint = "https://clamped.test/v1";
        hosted_efforts(endpoint);
        let sent = |model: &str, effort: &str| {
            clamp(&efforts(endpoint, model), effort).map(|level| level.id.clone())
        };
        assert_eq!(sent("gpt-5.5", "max").as_deref(), Some("xhigh"));
        assert_eq!(sent("gpt-5.5", "high").as_deref(), Some("high"));
        assert_eq!(sent("claude-opus-5", "none").as_deref(), Some("low"));
        assert_eq!(sent("claude-opus-5", "ultra").as_deref(), Some("max"));
        assert_eq!(sent("deepseek-v4-flash", "none").as_deref(), Some("none"));
        // As near to low as to high: the higher wins.
        assert_eq!(sent("deepseek-v4-flash", "medium").as_deref(), Some("high"));
        // Automatic, and a word that is no level, send nothing.
        assert_eq!(sent("gpt-5.5", ""), None);
        assert_eq!(sent("gpt-5.5", "turbo"), None);
    }

    #[test]
    fn a_model_without_levels_keeps_the_old_choices() {
        let endpoint = "https://levelless.test/v1";
        let list = json!([{"id": "some-model", "context_length": 32_000}]);
        remember(endpoint, list.as_array().unwrap());
        for model in ["some-model", "a-model-never-listed"] {
            let levels = efforts(endpoint, model);
            assert_eq!(ids(&levels), ["low", "medium", "high"]);
            assert_eq!(
                clamp(&levels, "medium").map(|l| l.id.as_str()),
                Some("medium")
            );
            // Saved on a model that goes further than these.
            assert_eq!(clamp(&levels, "max").map(|l| l.id.as_str()), Some("high"));
            assert_eq!(clamp(&levels, ""), None);
        }
    }
}
