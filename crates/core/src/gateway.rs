//! The gateways a session can send to, as the settings, `/gateways` and the
//! model picker list them: the providers in `providers::PROVIDERS`, and
//! those plugins add while they are on. What sets each one apart -- its URL,
//! what it needs, what it speaks -- is its provider's; this keeps the shape
//! the callers were written against.

use crate::providers::{AuthKind, PROVIDERS};
use std::collections::HashMap;
use std::sync::{LazyLock, Mutex, OnceLock};

/// The hosted pool on your Letronna account: no key, charged to your credit.
/// First in the list, and what a new install starts on.
pub const LETRONNA: &str = "Letronna";

pub struct Gateway {
    pub name: &'static str,
    pub base: &'static str,
    pub needs_key: bool,
    pub blurb: &'static str,
}

pub static GATEWAYS: LazyLock<Vec<Gateway>> = LazyLock::new(|| {
    PROVIDERS
        .iter()
        .map(|p| {
            let info = p.info();
            Gateway {
                name: info.name,
                base: info.base,
                needs_key: p.auth() == AuthKind::ApiKey,
                blurb: info.blurb,
            }
        })
        .collect()
});

/// What the built-in gateway `name` needs, `None` for one a plugin added.
fn auth_of(name: &str) -> Option<AuthKind> {
    crate::providers::find(name).map(|p| p.auth())
}

/// A gateway a plugin of the window serves while it is on, as the Codex
/// plugin serves a ChatGPT plan's models: listed and resolved like a
/// built-in one, in the process the plugin runs in only.
#[derive(Clone, Debug, PartialEq)]
pub struct Added {
    pub name: &'static str,
    pub base: String,
    pub key: String,
    /// Every model on it is sent the Responses API, which it alone speaks.
    pub responses: bool,
}

/// The gateways plugins add. Kept here rather than in the window's plugin
/// registry because requests resolve on worker threads with no `App` to hand.
fn added() -> &'static Mutex<Vec<Added>> {
    static ADDED: OnceLock<Mutex<Vec<Added>>> = OnceLock::new();
    ADDED.get_or_init(Default::default)
}

/// Offer `gateway`, in place of any a plugin added under its name before.
pub fn add(gateway: Added) {
    if let Ok(mut list) = added().lock() {
        list.retain(|g| g.name != gateway.name);
        list.push(gateway);
    }
}

/// Stop offering the gateway a plugin added as `name`.
pub fn remove(name: &str) {
    if let Ok(mut list) = added().lock() {
        list.retain(|g| g.name != name);
    }
}

fn added_as(name: &str) -> Option<Added> {
    added()
        .lock()
        .ok()?
        .iter()
        .find(|g| g.name == name)
        .cloned()
}

/// Whether `name` is a gateway on offer now: a built-in one, or one a plugin
/// added and has not taken back.
pub fn offered(name: &str) -> bool {
    GATEWAYS.iter().any(|g| g.name == name) || added_as(name).is_some()
}

/// Whether requests to `endpoint` go to the Responses API whatever the model,
/// because a plugin added a gateway there that speaks nothing else.
pub(crate) fn responses_at(endpoint: &str) -> bool {
    let endpoint = endpoint.trim().trim_end_matches('/');
    added().lock().is_ok_and(|list| {
        list.iter()
            .any(|g| g.responses && g.base.trim_end_matches('/') == endpoint)
    })
}

/// Where to send requests and what to send with them. A built-in gateway
/// goes to its own URL with its saved key; Custom goes to `custom`, the URL
/// that was pasted for it; the Letronna gateway uses the account API and the
/// token this computer was signed in with, so there is nothing to paste and
/// nothing to go stale.
pub fn resolve(name: &str, custom: &str, key: &str) -> (String, String) {
    if auth_of(name) == Some(AuthKind::Account) {
        let token = crate::auth::account().map(|a| a.token).unwrap_or_default();
        return (base_url(name, custom), token);
    }
    if let Some(added) = added_as(name) {
        return (added.base, added.key);
    }
    (base_url(name, custom), key.to_owned())
}

/// Where `resolve` sends a gateway's requests, without reading the account
/// file for its token, so a page can ask on every frame.
pub fn base_url(name: &str, custom: &str) -> String {
    if auth_of(name) == Some(AuthKind::Account) {
        return crate::auth::hosted_base();
    }
    if let Some(added) = added_as(name) {
        return added.base;
    }
    let base = gateway(name).base;
    let base = if base.is_empty() { custom } else { base };
    base.to_owned()
}

/// Every gateway set up on this computer, as (name, base URL, key): Letronna
/// once signed in, a keyed gateway once its key is saved, Custom once it has
/// a URL, Ollama always (on this machine, a closed port answers at once),
/// and those plugins added.
pub fn ready(keys: &HashMap<String, String>, custom: &str) -> Vec<(&'static str, String, String)> {
    let plugins = added().lock().map(|list| list.clone()).unwrap_or_default();
    let plugins = plugins.into_iter().map(|g| (g.name, g.base, g.key));
    GATEWAYS
        .iter()
        .filter_map(|g| {
            let key = keys.get(key_slot(g.name)).cloned().unwrap_or_default();
            let on = match auth_of(g.name) {
                Some(AuthKind::Account) => crate::auth::account().is_some(),
                Some(AuthKind::Url) => !custom.trim().is_empty(),
                Some(AuthKind::None) => true,
                _ => !key.trim().is_empty(),
            };
            on.then(|| {
                let (base, key) = resolve(g.name, custom, &key);
                (g.name, base, key)
            })
        })
        .chain(plugins)
        .collect()
}

/// Where a gateway stands, for the settings list and `/gateways`:
/// (status line, working, set up). `models` is how many it answered with.
pub fn status(
    name: &str,
    keys: &HashMap<String, String>,
    custom: &str,
    models: usize,
) -> (String, bool, bool) {
    let g = gateway(name);
    let count = |n: usize| format!("{n} model{}", if n == 1 { "" } else { "s" });
    let key = keys
        .get(key_slot(name))
        .is_some_and(|k| !k.trim().is_empty());
    let signed_in = crate::auth::account().is_some();
    match auth_of(name) {
        Some(AuthKind::Account) if signed_in => (
            if models > 0 {
                format!("Signed in · {}", count(models))
            } else {
                "Signed in".into()
            },
            true,
            true,
        ),
        Some(AuthKind::Account) => ("Not signed in".into(), false, false),
        Some(AuthKind::None) if models > 0 => (format!("Running · {}", count(models)), true, true),
        Some(AuthKind::None) => ("Not running".into(), false, false),
        Some(AuthKind::Url) if custom.trim().is_empty() => ("Not set up".into(), false, false),
        _ if g.needs_key && !key => ("Not set up".into(), false, false),
        _ if models > 0 => (format!("Connected · {}", count(models)), true, true),
        _ => ("Set up · no models answered".into(), false, true),
    }
}

/// The models of every ready gateway, asked all at once, as (gateway, model).
/// A gateway that does not answer is left out and its error returned, except
/// Ollama, which is only there when it is running.
pub fn all_models(
    keys: &HashMap<String, String>,
    custom: &str,
) -> (Vec<(String, String)>, Vec<String>) {
    let asks: Vec<_> = ready(keys, custom)
        .into_iter()
        .map(|(name, base, key)| {
            (
                name,
                std::thread::spawn(move || match crate::providers::find(name) {
                    Some(p) => p.models(&base, &key),
                    None => crate::chat::list_models(&base, &key),
                }),
            )
        })
        .collect();
    let (mut models, mut errors) = (Vec::new(), Vec::new());
    for (name, ask) in asks {
        match ask.join().unwrap_or_else(|_| Err("stopped".into())) {
            Ok(list) => models.extend(list.into_iter().map(|m| (name.to_owned(), m))),
            // A local server is only there while it runs.
            Err(_) if auth_of(name) == Some(AuthKind::None) => {}
            Err(e) => errors.push(format!("{name}: {e}")),
        }
    }
    (models, errors)
}

/// Where a gateway's key is saved: under its own name.
pub fn key_slot(name: &str) -> &str {
    name
}

pub fn gateway(name: &str) -> &'static Gateway {
    GATEWAYS
        .iter()
        .find(|g| g.name == name)
        .unwrap_or(&GATEWAYS[0])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_model_goes_to_its_own_gateway_and_custom_keeps_its_url() {
        assert_eq!(
            resolve("OpenRouter", "http://x/v1", "k").0,
            "https://openrouter.ai/api/v1"
        );
        assert_eq!(
            resolve("Custom", "http://x/v1", "k"),
            ("http://x/v1".into(), "k".into())
        );

        let keys = HashMap::from([("OpenAI".to_string(), "sk".to_string())]);
        let names: Vec<_> = ready(&keys, "").into_iter().map(|r| r.0).collect();
        assert!(names.contains(&"OpenAI") && names.contains(&"Ollama"));
        assert!(!names.contains(&"OpenRouter") && !names.contains(&"Custom"));
    }

    #[test]
    fn an_anthropic_key_goes_to_anthropic_itself() {
        let (base, key) = resolve("Anthropic", "", "sk-ant");
        assert_eq!(
            (base.as_str(), key.as_str()),
            ("https://api.anthropic.com/v1", "sk-ant")
        );
        let keys = HashMap::from([("Anthropic".to_string(), "sk-ant".to_string())]);
        let names: Vec<_> = ready(&keys, "").into_iter().map(|r| r.0).collect();
        assert!(names.contains(&"Anthropic"));
        let none = HashMap::new();
        assert_eq!(
            status("Anthropic", &none, "", 0),
            ("Not set up".into(), false, false)
        );
    }

    #[test]
    fn a_gateway_a_plugin_adds_is_offered_until_it_is_taken_back() {
        let base = "http://127.0.0.1:9/plugged/v1";
        assert!(!offered("Plugged"));
        add(Added {
            name: "Plugged",
            base: base.into(),
            key: "k".into(),
            responses: true,
        });
        assert!(offered("Plugged") && responses_at(&format!("{base}/")));
        assert_eq!(resolve("Plugged", "", ""), (base.into(), "k".into()));
        let listed = ready(&HashMap::new(), "");
        assert!(listed.contains(&("Plugged", base.into(), "k".into())));

        remove("Plugged");
        assert!(!offered("Plugged") && !responses_at(base));
        assert!(!ready(&HashMap::new(), "").iter().any(|g| g.0 == "Plugged"));
    }

    #[test]
    fn status_says_what_is_set_up_and_working() {
        let none = HashMap::new();
        let keyed = HashMap::from([("OpenAI".to_string(), "sk".to_string())]);
        assert_eq!(
            status("OpenAI", &none, "", 0),
            ("Not set up".into(), false, false)
        );
        assert_eq!(
            status("OpenAI", &keyed, "", 0),
            ("Set up · no models answered".into(), false, true)
        );
        assert_eq!(
            status("OpenAI", &keyed, "", 3),
            ("Connected · 3 models".into(), true, true)
        );
        assert_eq!(
            status("Ollama", &none, "", 1),
            ("Running · 1 model".into(), true, true)
        );
        assert!(!status("Custom", &none, "", 0).2);
        assert_eq!(
            status("Custom", &none, "http://x/v1", 2).0,
            "Connected · 2 models"
        );
    }
}
