//! The gateways a session can send to, as the settings, `/gateways` and the
//! model picker list them: the providers in `providers::all()`, those the
//! user added, and those plugins add while they are on. What sets each
//! built-in one apart -- its URL, what it needs, what it speaks -- is its
//! provider's; this keeps the shape the callers were written against.
//!
//! Custom takes one address. Someone with several servers of their own -- LM
//! Studio here, vLLM on another machine, a company proxy -- adds each as a
//! gateway with a name: any server that answers as OpenAI's API does, at
//! the address given, its key (if it takes one) kept as a gateway's is,
//! under `gateway:` and its name, so it never meets a search service's. They
//! are kept in gateways.json and listed after the built-in ones.

use crate::providers::AuthKind;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::SystemTime;

/// The hosted pool on your Letronna account: no key, charged to your credit.
/// First in the list, and what a new install starts on.
pub const LETRONNA: &str = "Letronna";

#[derive(Clone)]
pub struct Gateway {
    pub name: &'static str,
    pub base: &'static str,
    pub needs_key: bool,
    pub blurb: &'static str,
}

/// Every gateway: the built-in providers', those plugins declare, then the
/// user's own.
pub fn gateways() -> Vec<Gateway> {
    let own = own().into_iter().map(|g| as_gateway(&g));
    crate::providers::all()
        .into_iter()
        .map(|p| {
            let info = p.info();
            Gateway {
                name: info.name,
                base: info.base,
                needs_key: p.auth() == AuthKind::ApiKey,
                blurb: info.blurb,
            }
        })
        .chain(own)
        .collect()
}

/// What a gateway of the user's own is said to be.
const OWN_BLURB: &str = "Your own: any server that answers as OpenAI's API does.";

/// A gateway of the user's own as the lists show it. Its address is its
/// own, as Custom's is, so `base` is empty.
fn as_gateway(own: &Own) -> Gateway {
    Gateway {
        name: own.name,
        base: "",
        needs_key: false,
        blurb: OWN_BLURB,
    }
}

/// A gateway the user added, with the address its requests go to.
#[derive(Clone, Debug, PartialEq)]
pub struct Own {
    pub name: &'static str,
    pub base: String,
}

/// A gateway of the user's own as gateways.json keeps it.
#[derive(Serialize, Deserialize)]
struct Kept {
    name: String,
    base: String,
}

fn own_path() -> PathBuf {
    crate::chat::data_dir().join("gateways.json")
}

/// The gateways the user added, as gateways.json has them now.
pub fn own() -> Vec<Own> {
    // Asked for every gateway listed, every frame a page shows them, so the
    // file is read again only once it has changed: the terminal and the
    // window each save it, and each has to see the other's change.
    type Seen = (PathBuf, Option<(SystemTime, u64)>, Vec<Own>);
    static SEEN: Mutex<Option<Seen>> = Mutex::new(None);
    let path = own_path();
    let stamp = std::fs::metadata(&path)
        .ok()
        .and_then(|meta| Some((meta.modified().ok()?, meta.len())));
    if let Ok(seen) = SEEN.lock()
        && let Some((at, was, list)) = seen.as_ref()
        && *at == path
        && *was == stamp
    {
        return list.clone();
    }
    let list = read_own(&path);
    if let Ok(mut seen) = SEEN.lock() {
        *seen = Some((path, stamp, list.clone()));
    }
    list
}

fn read_own(path: &Path) -> Vec<Own> {
    let Ok(bytes) = std::fs::read(path) else {
        return Vec::new();
    };
    let kept: Vec<Kept> = serde_json::from_slice(&bytes).unwrap_or_default();
    let mut list: Vec<Own> = Vec::new();
    for gateway in kept {
        let name = gateway.name.trim();
        // A file edited by hand can name one twice, or a built-in one.
        if name.is_empty() || taken(name, &list) {
            continue;
        }
        list.push(Own {
            name: interned(name),
            base: gateway.base.trim().to_owned(),
        });
    }
    list
}

fn write_own(list: &[Own]) -> Result<(), String> {
    let kept: Vec<Kept> = list
        .iter()
        .map(|g| Kept {
            name: g.name.to_owned(),
            base: g.base.clone(),
        })
        .collect();
    let _ = std::fs::create_dir_all(crate::chat::data_dir());
    let bytes = serde_json::to_vec_pretty(&kept).map_err(|e| e.to_string())?;
    std::fs::write(own_path(), bytes).map_err(|e| format!("Cannot save the gateways: {e}"))
}

fn own_as(name: &str) -> Option<Own> {
    own().into_iter().find(|g| g.name == name)
}

/// Whether `name` is a gateway the user added.
pub fn is_own(name: &str) -> bool {
    own_as(name).is_some()
}

/// Add a gateway of the user's own called `name` at `address`, or give the
/// one already called that the new address. The name and address as they
/// are kept, or why they cannot be.
pub fn keep_own(name: &str, address: &str) -> Result<(&'static str, String), String> {
    let name = name.trim();
    let base = own_base(address)?;
    let mut list = own();
    let kept = match list.iter_mut().find(|g| g.name == name) {
        Some(gateway) => {
            gateway.base = base.clone();
            gateway.name
        }
        None => {
            check_name(name, &list)?;
            let name = interned(name);
            list.push(Own {
                name,
                base: base.clone(),
            });
            name
        }
    };
    write_own(&list)?;
    Ok((kept, base))
}

/// Stop offering the gateway of the user's own called `name`, and forget
/// its key.
pub fn remove_own(name: &str) -> Result<(), String> {
    let mut list = own();
    let Some(gone) = list.iter().position(|g| g.name == name) else {
        return Ok(());
    };
    let slot = key_slot(name).to_owned();
    list.remove(gone);
    write_own(&list)?;
    crate::store::save_key(&slot, None)
        .map(|_| ())
        .map_err(|e| format!("The gateway is gone, but its key could not be removed: {e}."))
}

/// The most characters a gateway's name may have: enough for "Office vLLM
/// on the big box", short enough for the lists.
const NAME_CHARS: usize = 40;

/// Why `name` cannot be a new gateway's, if it cannot: empty, too long,
/// holding what the lists and fallbacks write between a gateway and its
/// model, or a gateway's already.
fn check_name(name: &str, list: &[Own]) -> Result<(), String> {
    if name.is_empty() {
        return Err("Give the gateway a name, such as LM Studio.".into());
    }
    if name.chars().count() > NAME_CHARS {
        return Err(format!(
            "A gateway's name is {NAME_CHARS} characters at most."
        ));
    }
    if name
        .chars()
        .any(|c| c.is_control() || matches!(c, ':' | '·'))
    {
        return Err("A gateway's name cannot hold a colon or a dot in the middle (·).".into());
    }
    if taken(name, list) {
        return Err(format!(
            "There is a gateway called {name} already: pick another name."
        ));
    }
    Ok(())
}

/// Whether a built-in gateway, one a plugin added, or one of `list` is
/// called `name`, in any case.
fn taken(name: &str, list: &[Own]) -> bool {
    let same = |other: &str| other.eq_ignore_ascii_case(name);
    crate::providers::all().iter().any(|p| same(p.info().name))
        || list.iter().any(|g| same(g.name))
        || added()
            .lock()
            .is_ok_and(|added| added.iter().any(|g| same(g.name)))
}

/// `pasted` as a gateway's address is kept: no slash or chat route on the
/// end, and with no scheme, http for this computer and the local network
/// and https for anywhere else. Checked as a request to it would be.
pub fn own_base(pasted: &str) -> Result<String, String> {
    let mut base = pasted.trim().trim_end_matches('/').to_owned();
    if let Some(cut) = base.strip_suffix("/chat/completions") {
        base = cut.trim_end_matches('/').to_owned();
    }
    if base.is_empty() {
        return Err("Give the server's base URL, such as http://localhost:1234/v1.".into());
    }
    if !base.contains("://") {
        let near =
            reqwest::Url::parse(&format!("http://{base}")).is_ok_and(|url| crate::web::local(&url));
        let scheme = if near { "http" } else { "https" };
        base = format!("{scheme}://{base}");
    }
    let probe = crate::chat::Connection {
        endpoint: base.clone(),
        model: "any".into(),
        key: "any".into(),
        ..Default::default()
    };
    crate::chat::validate(&probe)?;
    Ok(base)
}

/// `name` for as long as the process runs. The names of the user's own
/// gateways are read from a file but go where a built-in one's would, and
/// each is kept once, however often the file is read.
fn interned(name: &str) -> &'static str {
    static NAMES: OnceLock<Mutex<HashSet<&'static str>>> = OnceLock::new();
    let names = NAMES.get_or_init(Default::default);
    let Ok(mut names) = names.lock() else {
        return Box::leak(name.to_owned().into_boxed_str());
    };
    if let Some(kept) = names.get(name) {
        return kept;
    }
    let kept: &'static str = Box::leak(name.to_owned().into_boxed_str());
    names.insert(kept);
    kept
}

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

/// Whether `name` is a gateway on offer now: a built-in one whose plugin, if
/// it has one, is on, or one a plugin added and has not taken back.
pub fn offered(name: &str) -> bool {
    (gateways().iter().any(|g| g.name == name) && crate::providers::offered(name))
        || added_as(name).is_some()
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
    // A browser sign-in sends its access token, refreshed when near its end.
    if let Some(AuthKind::OAuth(_)) = auth_of(name) {
        let token = crate::providers::auth::access(name).unwrap_or_default();
        return (base_url(name, custom), token);
    }
    if let Some(added) = added_as(name) {
        return (added.base, added.key);
    }
    if let Some(own) = own_as(name) {
        return (own.base, key.to_owned());
    }
    (base_url(name, custom), key.to_owned())
}

/// The gateway whose requests go to `endpoint`, if one does; Custom, whose
/// address is the user's, is never named, but one the user added and named
/// is.
pub fn named(endpoint: &str) -> Option<&'static str> {
    let endpoint = endpoint.trim().trim_end_matches('/');
    let at = |base: &str| !base.is_empty() && base.trim_end_matches('/') == endpoint;
    let plugin = added()
        .lock()
        .ok()
        .and_then(|list| list.iter().find(|g| at(&g.base)).map(|g| g.name));
    plugin.or_else(|| {
        gateways()
            .iter()
            .find(|g| at(&base_url(g.name, "")))
            .map(|g| g.name)
    })
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
    if let Some(own) = own_as(name) {
        return own.base;
    }
    let base = gateway(name).base;
    let base = if base.is_empty() { custom } else { base };
    base.to_owned()
}

/// Every gateway set up on this computer, as (name, base URL, key): Letronna
/// and those signed in to in the browser once signed in, a keyed gateway
/// once its key is saved, Custom once it has a URL, the user's own, Ollama
/// always (on this machine, a closed port answers at once), and those
/// plugins added. One a plugin offers is left out while it is off.
pub fn ready(keys: &HashMap<String, String>, custom: &str) -> Vec<(&'static str, String, String)> {
    let plugins = added().lock().map(|list| list.clone()).unwrap_or_default();
    let plugins = plugins.into_iter().map(|g| (g.name, g.base, g.key));
    gateways()
        .iter()
        .filter(|g| crate::providers::offered(g.name))
        .filter_map(|g| {
            let key = keys.get(key_slot(g.name)).cloned().unwrap_or_default();
            let on = match auth_of(g.name) {
                Some(AuthKind::Account) => crate::auth::account().is_some(),
                Some(AuthKind::OAuth(_)) => crate::providers::auth::signed_in(g.name),
                Some(AuthKind::Url) => !base_url(g.name, custom).trim().is_empty(),
                Some(AuthKind::None) => true,
                // One of the user's own, at the address it was added with.
                None => !base_url(g.name, custom).trim().is_empty(),
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
        Some(AuthKind::OAuth(_)) if crate::providers::auth::signed_in(name) => (
            if models > 0 {
                format!("Signed in · {}", count(models))
            } else {
                "Signed in".into()
            },
            true,
            true,
        ),
        Some(AuthKind::OAuth(_)) => ("Not signed in".into(), false, false),
        Some(AuthKind::None) if models > 0 => (format!("Running · {}", count(models)), true, true),
        Some(AuthKind::None) => ("Not running".into(), false, false),
        Some(AuthKind::Url) if base_url(name, custom).trim().is_empty() => {
            ("Not set up".into(), false, false)
        }
        None if base_url(name, custom).trim().is_empty() => ("Not set up".into(), false, false),
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

/// The models `name` offers, asked now as `all_models` asks each gateway,
/// for a check right after it is set up; or what it said instead.
pub fn models_of(
    name: &str,
    keys: &HashMap<String, String>,
    custom: &str,
) -> Result<Vec<String>, String> {
    let key = keys.get(key_slot(name)).cloned().unwrap_or_default();
    let (base, key) = resolve(name, custom, &key);
    if base.trim().is_empty() {
        return Err(format!("{name} has no address yet."));
    }
    match crate::providers::find(name) {
        Some(p) => p.models(&base, &key),
        None => crate::chat::list_models(&base, &key),
    }
}

/// Where a gateway's key is saved: under its own name, or for one of the
/// user's own under `gateway:` and its name, which no search service or
/// other key is ever called.
pub fn key_slot(name: &str) -> &str {
    match own_as(name) {
        Some(own) => interned(&format!("gateway:{}", own.name)),
        None => name,
    }
}

/// The gateway called `name`; the first, Letronna, when none is.
pub fn gateway(name: &str) -> Gateway {
    let mut all = gateways();
    let at = all.iter().position(|g| g.name == name).unwrap_or(0);
    all.swap_remove(at)
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

    #[test]
    fn a_provider_a_plugin_offers_is_there_only_while_the_plugin_is_on() {
        let none = HashMap::new();
        let _vault = crate::providers::auth::tests::vault("gateway-plugin");
        crate::providers::auth::tests::keep("ChatGPT", "access-1");
        crate::providers::save_switch("chatgpt", false);
        // Signed in, but its plugin is switched off.
        assert!(!offered("ChatGPT"));
        assert!(!ready(&none, "").iter().any(|g| g.0 == "ChatGPT"));
        crate::providers::tests::plugged_in("chatgpt");
        assert!(offered("ChatGPT"));
        assert!(ready(&none, "").iter().any(|g| g.0 == "ChatGPT"));
    }

    /// A data folder of a test's own while `run` runs, as gateways.json and
    /// the keys are kept there.
    fn in_scratch<T>(name: &str, run: impl FnOnce(&std::path::Path) -> T) -> T {
        let _turn = crate::store::tests::DATA_DIR
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let dir = crate::store::tests::Scratch::new(name);
        unsafe { std::env::set_var("AGENT_DATA_DIR", &*dir) };
        let done = run(&dir);
        unsafe { std::env::remove_var("AGENT_DATA_DIR") };
        done
    }

    #[test]
    fn gateways_of_your_own_are_listed_resolved_and_kept_apart() {
        in_scratch("own-gateways", |_| {
            assert!(own().is_empty());
            let (studio, base) = keep_own(" LM Studio ", "localhost:1234/v1/").unwrap();
            assert_eq!(
                (studio, base.as_str()),
                ("LM Studio", "http://localhost:1234/v1")
            );
            let (vllm, _) = keep_own("vLLM", "https://gpu.example/v1/chat/completions").unwrap();
            assert_eq!(base_url(vllm, ""), "https://gpu.example/v1");
            // Listed after the built-in ones, and offered.
            let names: Vec<&str> = gateways().iter().map(|g| g.name).collect();
            assert_eq!(names[names.len() - 2..], ["LM Studio", "vLLM"]);
            assert!(offered("LM Studio") && is_own("vLLM") && !is_own("OpenAI"));
            assert_eq!(gateway("vLLM").name, "vLLM");
            // Its key is kept where no search service's or other key is.
            assert_eq!(key_slot("vLLM"), "gateway:vLLM");
            assert_eq!(key_slot("OpenAI"), "OpenAI");
            let keys = HashMap::from([("gateway:vLLM".to_string(), "k".to_string())]);
            assert_eq!(
                resolve("vLLM", "", "k"),
                ("https://gpu.example/v1".into(), "k".into())
            );
            let ready = ready(&keys, "");
            assert!(ready.contains(&("vLLM", "https://gpu.example/v1".into(), "k".into())));
            assert!(ready.contains(&(
                "LM Studio",
                "http://localhost:1234/v1".into(),
                String::new()
            )));
            assert_eq!(named("https://gpu.example/v1/"), Some("vLLM"));
            assert_eq!(
                status("vLLM", &keys, "", 4),
                ("Connected · 4 models".into(), true, true)
            );
            // A new address for one kept, then gone.
            keep_own("vLLM", "https://gpu2.example/v1").unwrap();
            assert_eq!(base_url("vLLM", ""), "https://gpu2.example/v1");
            // Asked for its models, a server that is not there says so.
            assert!(models_of("LM Studio", &keys, "").is_err());
            remove_own("vLLM").unwrap();
            assert!(!offered("vLLM") && own().len() == 1);
        });
    }

    #[test]
    fn a_gateway_of_your_own_needs_a_name_of_its_own_and_an_address_that_works() {
        in_scratch("own-gateway-names", |_| {
            assert!(keep_own("", "http://localhost:1/v1").is_err());
            assert!(
                keep_own("openai", "http://localhost:1/v1")
                    .unwrap_err()
                    .contains("already")
            );
            assert!(keep_own("Custom", "http://localhost:1/v1").is_err());
            assert!(keep_own("a:b", "http://localhost:1/v1").is_err());
            assert!(keep_own(&"x".repeat(41), "http://localhost:1/v1").is_err());
            assert!(keep_own("Box", "").is_err());
            keep_own("Box", "http://localhost:1/v1").unwrap();
            assert!(
                keep_own("box", "http://localhost:2/v1")
                    .unwrap_err()
                    .contains("already")
            );
            // Plain http goes only to this computer unless the user opts in.
            assert!(keep_own("Far", "http://gpu.example/v1").is_err());
            assert_eq!(
                own_base("gpu.example/v1"),
                Ok("https://gpu.example/v1".into())
            );
            assert_eq!(
                own_base("127.0.0.1:8000/v1"),
                Ok("http://127.0.0.1:8000/v1".into())
            );
        });
    }

    #[test]
    fn a_hand_edited_list_of_your_own_gateways_skips_what_it_cannot_use() {
        in_scratch("own-gateways-edited", |dir| {
            std::fs::write(
                dir.join("gateways.json"),
                r#"[{"name": "Box", "base": "http://localhost:1/v1"},
                    {"name": "box", "base": "http://localhost:2/v1"},
                    {"name": "OpenAI", "base": "http://localhost:3/v1"},
                    {"name": " ", "base": "http://localhost:4/v1"}]"#,
            )
            .unwrap();
            let names: Vec<&str> = own().iter().map(|g| g.name).collect();
            assert_eq!(names, ["Box"]);
        });
    }

    #[test]
    fn a_browser_sign_in_is_set_up_once_signed_in() {
        let none = HashMap::new();
        assert_eq!(
            status("ChatGPT", &none, "", 0),
            ("Not signed in".into(), false, false)
        );
        assert!(!ready(&none, "").iter().any(|g| g.0 == "ChatGPT"));
        let _vault = crate::providers::auth::tests::vault("gateway-sign-in");
        crate::providers::tests::plugged_in("chatgpt");
        crate::providers::auth::tests::keep("ChatGPT", "access-1");
        assert_eq!(
            status("ChatGPT", &none, "", 2),
            ("Signed in · 2 models".into(), true, true)
        );
        let listed = ready(&none, "");
        let chatgpt = listed.iter().find(|g| g.0 == "ChatGPT").unwrap();
        assert_eq!(
            (chatgpt.1.as_str(), chatgpt.2.as_str()),
            ("https://chatgpt.com/backend-api/codex", "access-1")
        );
        crate::providers::auth::sign_out("ChatGPT").unwrap();
        assert!(!ready(&none, "").iter().any(|g| g.0 == "ChatGPT"));
    }
}
