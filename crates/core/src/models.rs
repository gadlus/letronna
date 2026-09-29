//! What Letronna knows about a model: how much it can read in one request,
//! how much it can write in one reply, what it takes in, and which
//! parameters it accepts.
//!
//! Three sources, cheapest first. The gateways' model lists already say most
//! of it -- OpenRouter gives the window, the output limit, the kinds of input
//! and the parameters, and the hosted gateway the window -- so listing models
//! keeps that instead of only the ids. Where a list says nothing, as
//! OpenAI's does, a small table of well-known families fills in the window
//! and the output limit by the longest prefix of the id. And a provider's
//! error can give away a limit lower than either, which is the one the route
//! actually enforces; what is learned that way only ever lowers what is known.
//!
//! Facts are kept per host and model, because one id served somewhere else
//! is another route: Ollama serves a smaller window than a model was trained
//! for, and a proxy may cap the output. They live in memory only and are put
//! together when asked, from what is already here. Nothing is fetched to
//! answer, so asking never holds up a request.

use serde_json::Value;
use std::{
    collections::HashMap,
    sync::{Mutex, OnceLock},
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

/// Everything learned so far, by (host, model).
#[derive(Default)]
struct Known {
    /// What each gateway's model list said.
    listed: HashMap<(String, String), Facts>,
    /// Limits from providers' errors; only `context` and `max_output` are set.
    learned: HashMap<(String, String), Facts>,
}

fn known() -> &'static Mutex<Known> {
    static KNOWN: OnceLock<Mutex<Known>> = OnceLock::new();
    KNOWN.get_or_init(Default::default)
}

/// What is known about `model` at `endpoint`, put together from memory.
pub fn facts(endpoint: &str, model: &str) -> Facts {
    let key = (host(endpoint), model.to_owned());
    let (listed, learned) = known()
        .lock()
        .map(|known| {
            (
                known.listed.get(&key).cloned().unwrap_or_default(),
                known.learned.get(&key).cloned().unwrap_or_default(),
            )
        })
        .unwrap_or_default();
    let family = family(model);
    Facts {
        context: lowest(listed.context.or(family.context), learned.context),
        max_output: lowest(listed.max_output.or(family.max_output), learned.max_output),
        inputs: listed.inputs,
        parameters: listed.parameters,
    }
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

/// Keep what a gateway's `/models` list says about each of its models.
pub(crate) fn remember(endpoint: &str, list: &[Value]) {
    let host = host(endpoint);
    let Ok(mut known) = known().lock() else {
        return;
    };
    for entry in list {
        let Some(id) = entry["id"].as_str().or_else(|| entry["key"].as_str()) else {
            continue;
        };
        known
            .listed
            .insert((host.clone(), id.to_owned()), read(entry));
    }
}

/// The facts in one entry of a `/models` list, in OpenRouter's shape, which
/// the hosted gateway follows for the window; OpenAI's entries hold none.
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
    Facts {
        context: tokens(&entry["context_length"])
            .or_else(|| tokens(&entry["top_provider"]["context_length"])),
        max_output: tokens(&entry["top_provider"]["max_completion_tokens"]),
        inputs: names(&entry["architecture"]["input_modalities"]),
        parameters: names(&entry["supported_parameters"]),
    }
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
    use serde_json::json;

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
    fn an_openai_list_says_nothing_so_a_known_family_fills_in() {
        let list = json!({"object": "list", "data": [
            {"id": "gpt-4o-2024-08-06", "object": "model", "created": 1_722_814_719, "owned_by": "system"},
            {"id": "text-embedding-3-small", "object": "model", "created": 1_705_948_997, "owned_by": "system"}
        ]});
        let endpoint = "https://openai.test/v1";
        remember(endpoint, list["data"].as_array().unwrap());

        let gpt = facts(endpoint, "gpt-4o-2024-08-06");
        assert_eq!((gpt.context, gpt.max_output), (Some(128_000), Some(16_384)));
        assert_eq!((gpt.inputs, gpt.parameters), (None, None));
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
    fn the_hosted_list_gives_the_window_and_the_family_the_output_limit() {
        let list = json!({"object": "list", "data": [
            {"id": "claude-sonnet-5", "object": "model", "owned_by": "Anthropic", "context_length": 500_000}
        ]});
        let endpoint = "https://hosted.test/v1";
        remember(endpoint, list["data"].as_array().unwrap());

        let sonnet = facts(endpoint, "claude-sonnet-5");
        assert_eq!(sonnet.context, Some(500_000));
        assert_eq!(sonnet.max_output, Some(128_000));
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
}
