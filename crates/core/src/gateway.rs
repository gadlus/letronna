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

/// gateways.json as it is, every line of it, those `own` leaves out too.
fn read_kept(path: &Path) -> Vec<Kept> {
    std::fs::read(path)
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

fn read_own(path: &Path) -> Vec<Own> {
    let mut list: Vec<Own> = Vec::new();
    for gateway in read_kept(path) {
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

/// Write `kept` to gateways.json. It is always the file as read with one
/// change made, never `own`'s list: a line that list leaves out -- one a
/// plugin's gateway of the same name hides while it is on -- stays.
fn write_kept(kept: &[Kept]) -> Result<(), String> {
    let _ = std::fs::create_dir_all(crate::chat::data_dir());
    let bytes = serde_json::to_vec_pretty(kept).map_err(|e| e.to_string())?;
    std::fs::write(own_path(), bytes).map_err(|e| format!("Cannot save the gateways: {e}"))
}

fn own_as(name: &str) -> Option<Own> {
    own().into_iter().find(|g| g.name == name)
}

/// Whether `name` is a gateway the user added.
pub fn is_own(name: &str) -> bool {
    own_as(name).is_some()
}

/// Add a gateway of the user's own called `name` at `address`. A name any
/// gateway has already, in any case, is refused, never taken to mean that
/// one: adding is never a way to change another. The name and address as
/// they are kept, or why they cannot be.
pub fn add_own(name: &str, address: &str) -> Result<(&'static str, String), String> {
    let name = name.trim();
    let base = own_base(address)?;
    check_name(name, &own())?;
    let mut kept = read_kept(&own_path());
    // A line of the file `own` left out still holds its name.
    if kept
        .iter()
        .any(|k| k.name.trim().eq_ignore_ascii_case(name))
    {
        return Err(already(name));
    }
    kept.push(Kept {
        name: name.to_owned(),
        base: base.clone(),
    });
    write_kept(&kept)?;
    crate::providers::router::forget();
    Ok((interned(name), base))
}

/// A name a new gateway called `name` could have instead, when a gateway
/// has that one: the first of "name 2", "name 3" and on that none has,
/// within the length a name may have. `None` while `name` is free.
pub fn free_name(name: &str) -> Option<String> {
    let name = name.trim();
    let list = own();
    let kept = read_kept(&own_path());
    let used = |name: &str| {
        taken(name, &list)
            || kept
                .iter()
                .any(|k| k.name.trim().eq_ignore_ascii_case(name))
    };
    if name.is_empty() || !used(name) {
        return None;
    }
    (2..1000)
        .map(|n| {
            let number = format!(" {n}");
            let room = NAME_CHARS.saturating_sub(number.chars().count());
            let stem: String = name.chars().take(room).collect();
            format!("{}{number}", stem.trim_end())
        })
        .find(|candidate| !used(candidate))
}

/// The gateway that has `name` already, in any case, as the lists spell it.
pub fn holder(name: &str) -> Option<&'static str> {
    gateways()
        .into_iter()
        .map(|g| g.name)
        .find(|other| other.eq_ignore_ascii_case(name.trim()))
}

/// Give the gateway of the user's own called `name` a new address: the
/// address as it is kept, or why it cannot be. A move to another host
/// forgets its key, which was given for the old one and would otherwise go
/// to the new one with the next request.
pub fn change_own(name: &str, address: &str) -> Result<String, String> {
    let base = own_base(address)?;
    let mut kept = read_kept(&own_path());
    let Some(gateway) = kept.iter_mut().find(|k| k.name.trim() == name) else {
        return Err(format!("There is no gateway of your own called {name}."));
    };
    let moved = !same_host(&gateway.base, &base);
    gateway.base = base.clone();
    write_kept(&kept)?;
    crate::providers::router::forget();
    if moved {
        crate::store::save_key(key_slot(name), None).map_err(|e| {
            format!("The address changed, but its old key could not be removed: {e}.")
        })?;
    }
    Ok(base)
}

/// Whether two addresses reach the same server: scheme, host and port, as
/// a browser decides which site a cookie goes to.
fn same_host(a: &str, b: &str) -> bool {
    let site = |address: &str| {
        reqwest::Url::parse(address).ok().map(|url| {
            (
                url.scheme().to_owned(),
                url.host_str().map(str::to_ascii_lowercase),
                url.port_or_known_default(),
            )
        })
    };
    matches!((site(a), site(b)), (Some(a), Some(b)) if a == b)
}

/// Stop offering the gateway of the user's own called `name`, and forget
/// its key.
pub fn remove_own(name: &str) -> Result<(), String> {
    if !is_own(name) {
        return Ok(());
    }
    let slot = key_slot(name).to_owned();
    let mut kept = read_kept(&own_path());
    kept.retain(|k| k.name.trim() != name);
    write_kept(&kept)?;
    crate::providers::router::forget();
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
        return Err(already(name));
    }
    Ok(())
}

/// What adding a gateway under a name one has already is answered, the
/// name spelled as that one's is.
fn already(name: &str) -> String {
    let spelled = holder(name).unwrap_or(name);
    format!("There is a gateway called {spelled} already: pick another name, or change that one.")
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
/// and https for anywhere else. Checked as a request to it would be, and
/// never Letronna's own, which the account reaches with its sign-in: under
/// another gateway it listed every model twice with a key that is not the
/// account's.
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
    if base == crate::auth::hosted_base() {
        return Err(format!(
            "That is Letronna's own address: sign in under {LETRONNA} to use its models."
        ));
    }
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
                std::thread::spawn(move || {
                    let listed = match crate::providers::find(name) {
                        Some(p) => p.models(&base, &key),
                        None => crate::chat::list_models(&base, &key),
                    };
                    // What the lists show is what the router reaches for, so
                    // a gateway just set up is tried for its models at once.
                    if let Ok(models) = &listed {
                        crate::providers::router::listed(&base, &key, models);
                    }
                    listed
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

/// Whether `model` on `name` is listed but cannot be picked or sent to: one
/// of the hosted pool's, while `account` has no plan or credit to pay for it
/// and no perk makes it free today.
pub fn needs_subscription(name: &str, model: &str, account: Option<&crate::auth::Account>) -> bool {
    match account.filter(|_| hosted_unpaid(name, account)) {
        Some(account) => !account.free_for("chat", maker(name, model, account).as_deref(), model),
        None => false,
    }
}

/// Whether `name` is the hosted pool and `account` has no plan or credit to
/// pay for its models, so that only those a perk makes free today are open:
/// what a list of them says over them, with the way to upgrade.
pub fn hosted_unpaid(name: &str, account: Option<&crate::auth::Account>) -> bool {
    auth_of(name) == Some(AuthKind::Account)
        && account.is_some_and(crate::auth::Account::needs_upgrade)
}

/// Whether a perk makes `model` on `name`, the hosted pool, free today, as
/// a list says beside it.
pub fn free_today(name: &str, model: &str, account: Option<&crate::auth::Account>) -> bool {
    auth_of(name) == Some(AuthKind::Account)
        && account.is_some_and(|account| {
            account.free_for("chat", maker(name, model, account).as_deref(), model)
        })
}

/// Whether `model` on `name` needs a subscription only because the perk
/// that makes it free has used up today's allowance, which comes back at
/// 00:00 UTC.
pub fn used_up_today(name: &str, model: &str, account: Option<&crate::auth::Account>) -> bool {
    match account.filter(|_| hosted_unpaid(name, account)) {
        Some(account) => {
            account.perk_used_up_for("chat", maker(name, model, account).as_deref(), model)
        }
        None => false,
    }
}

/// Who made `model` on `name`, as its list said once it was read; asked
/// only when a perk of `account`'s is for one maker's models, which alone
/// turn on it.
fn maker(name: &str, model: &str, account: &crate::auth::Account) -> Option<String> {
    (account.perks.iter())
        .any(|perk| perk.maker.is_some())
        .then(|| crate::models::facts(&base_url(name, ""), model).maker)
        .flatten()
}

/// Read the hosted pool's list now when who made `model` is not known yet
/// and a perk of `account`'s could turn on it, so `needs_subscription` can
/// tell: a terminal holding a message back may not have listed it.
pub fn learn_maker(model: &str, account: &crate::auth::Account) {
    let wanted = (account.perks.iter()).any(|perk| perk.maker.is_some());
    if wanted && maker(LETRONNA, model, account).is_none() {
        let _ = models_of(LETRONNA, &HashMap::new(), "");
    }
}

/// The first gateway among `models` that is not the hosted pool, to point to
/// while the hosted models it would be sent to need a subscription.
pub fn usable_elsewhere(models: &[(String, String)]) -> Option<&str> {
    (models.iter())
        .map(|(name, _)| name.as_str())
        .find(|name| auth_of(name) != Some(AuthKind::Account))
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
    let listed = match crate::providers::find(name) {
        Some(p) => p.models(&base, &key),
        None => crate::chat::list_models(&base, &key),
    };
    if let Ok(models) = &listed {
        crate::providers::router::listed(&base, &key, models);
    }
    listed
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
            let (studio, base) = add_own(" LM Studio ", "localhost:1234/v1/").unwrap();
            assert_eq!(
                (studio, base.as_str()),
                ("LM Studio", "http://localhost:1234/v1")
            );
            let (vllm, _) = add_own("vLLM", "https://gpu.example/v1/chat/completions").unwrap();
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
            change_own("vLLM", "https://gpu2.example/v1").unwrap();
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
            assert!(add_own("", "http://localhost:1/v1").is_err());
            assert!(
                add_own("openai", "http://localhost:1/v1")
                    .unwrap_err()
                    .contains("already")
            );
            assert!(add_own("Custom", "http://localhost:1/v1").is_err());
            assert!(add_own("a:b", "http://localhost:1/v1").is_err());
            assert!(add_own(&"x".repeat(41), "http://localhost:1/v1").is_err());
            assert!(add_own("Box", "").is_err());
            add_own("Box", "http://localhost:1/v1").unwrap();
            // The same name again, in any case, is refused, and the one
            // there keeps its address: adding never changes another.
            for again in ["Box", "box", " BOX "] {
                assert_eq!(
                    add_own(again, "http://localhost:2/v1").unwrap_err(),
                    "There is a gateway called Box already: pick another name, or change that one."
                );
            }
            assert_eq!(base_url("Box", ""), "http://localhost:1/v1");
            // What it could be called instead, and who has the name.
            assert_eq!(free_name(" box "), Some("box 2".into()));
            assert_eq!(free_name("OpenAI"), Some("OpenAI 2".into()));
            assert_eq!(free_name("Elsewhere"), None);
            assert_eq!(holder("BOX"), Some("Box"));
            add_own("Box 2", "http://localhost:2/v1").unwrap();
            assert_eq!(free_name("Box"), Some("Box 3".into()));
            let long = "y".repeat(40);
            add_own(&long, "http://localhost:2/v1").unwrap();
            let instead = free_name(&long).unwrap();
            assert!(instead.ends_with(" 2") && instead.chars().count() == 40);
            // Changing is its own step, and only for one that is there.
            assert_eq!(
                change_own("Box", "localhost:3/v1"),
                Ok("http://localhost:3/v1".into())
            );
            assert!(change_own("Nobody", "http://localhost:3/v1").is_err());
            // Plain http goes only to this computer unless the user opts in.
            assert!(add_own("Far", "http://gpu.example/v1").is_err());
            assert_eq!(
                own_base("gpu.example/v1"),
                Ok("https://gpu.example/v1".into())
            );
            assert_eq!(
                own_base("127.0.0.1:8000/v1"),
                Ok("http://127.0.0.1:8000/v1".into())
            );
            let hosted = format!("{}/chat/completions", crate::auth::hosted_base());
            assert!(
                own_base(&hosted)
                    .unwrap_err()
                    .contains("sign in under Letronna")
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
            // What was left out stays in the file as written, as a line a
            // plugin's gateway hides while it is on must come back after,
            // and its name is still not free.
            assert!(add_own("openai", "http://localhost:5/v1").is_err());
            add_own("Two", "http://localhost:5/v1").unwrap();
            remove_own("Box").unwrap();
            let file = std::fs::read_to_string(dir.join("gateways.json")).unwrap();
            assert!(file.contains("\"OpenAI\"") && file.contains("\"box\""));
            assert!(!file.contains("\"Box\""));
            let names: Vec<&str> = own().iter().map(|g| g.name).collect();
            assert_eq!(names, ["box", "Two"]);
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

    #[test]
    fn hosted_models_need_a_subscription_only_on_free_with_no_credit() {
        use crate::auth::{Account, Team};
        let free = Account {
            plan: "free".into(),
            ..Default::default()
        };
        fn locked(name: &str, account: Option<&Account>) -> bool {
            needs_subscription(name, "hosted", account)
        }
        assert!(locked(LETRONNA, Some(&free)));
        assert!(hosted_unpaid(LETRONNA, Some(&free)));
        // Only the hosted pool's: a key or a server of your own is yours.
        assert!(!locked("Custom", Some(&free)));
        assert!(!locked("OpenAI", Some(&free)));
        assert!(!hosted_unpaid("OpenAI", Some(&free)));
        assert!(!locked(LETRONNA, None));
        let credit = Account {
            credits: 500,
            ..free.clone()
        };
        assert!(!locked(LETRONNA, Some(&credit)));
        let pro = Account {
            plan: "pro".into(),
            ..Default::default()
        };
        assert!(!locked(LETRONNA, Some(&pro)));
        // Never checked with the API, so no plan is known yet.
        assert!(!locked(LETRONNA, Some(&Account::default())));
        // A team's pool pays when the bills go to it, and only then.
        let team = |credits| Account {
            credits: 500,
            team: Some(Team {
                credits,
                ..Default::default()
            }),
            ..free.clone()
        };
        assert!(!locked(LETRONNA, Some(&team(900))));
        assert!(locked(LETRONNA, Some(&team(0))));

        let models = [
            (LETRONNA.to_owned(), "hosted".to_owned()),
            ("Custom".to_owned(), "local".to_owned()),
        ];
        assert_eq!(usable_elsewhere(&models), Some("Custom"));
        assert_eq!(usable_elsewhere(&models[..1]), None);
    }

    /// A Free plan with no credit, given these perks as `/me` lists them.
    fn perked(perks: serde_json::Value) -> crate::auth::Account {
        crate::auth::Account {
            plan: "free".into(),
            perks: serde_json::from_value(perks).unwrap(),
            ..Default::default()
        }
    }

    #[test]
    fn a_model_a_perk_makes_free_today_stays_open_on_a_free_plan() {
        let account = perked(serde_json::json!([
            {"id": "f", "name": "Fast", "kind": "chat", "model": "letronna/fast",
             "share": 100, "available": true},
            {"id": "s", "name": "Smart", "kind": "chat", "model": "letronna/smart",
             "share": 100, "available": false},
            {"id": "h", "name": "Half", "kind": "chat", "model": "letronna/half",
             "share": 50, "available": true},
        ]));
        let account = Some(&account);
        assert!(!needs_subscription(LETRONNA, "letronna/fast", account));
        assert!(free_today(LETRONNA, "letronna/fast", account));
        // Its allowance used up, it is closed until 00:00 UTC, and says why.
        assert!(needs_subscription(LETRONNA, "letronna/smart", account));
        assert!(used_up_today(LETRONNA, "letronna/smart", account));
        assert!(!free_today(LETRONNA, "letronna/smart", account));
        // Half off still costs something, and there is no credit.
        assert!(needs_subscription(LETRONNA, "letronna/half", account));
        assert!(!used_up_today(LETRONNA, "letronna/half", account));
        assert!(needs_subscription(LETRONNA, "letronna/deep", account));
        assert!(!used_up_today(LETRONNA, "letronna/deep", account));
        // The pool is still one to upgrade for, as its heading says.
        assert!(hosted_unpaid(LETRONNA, account));
        // A perk is for the hosted pool's models alone.
        assert!(!free_today("Custom", "letronna/fast", account));
    }

    #[test]
    fn a_perk_for_one_makers_models_goes_by_what_the_list_says() {
        let account = perked(serde_json::json!([
            {"id": "q", "name": "Qwen", "kind": "chat", "maker": "Qwen", "share": 100,
             "available": true},
        ]));
        let account = Some(&account);
        // Before the list is read, who made it is not known.
        assert!(needs_subscription(LETRONNA, "qwen-perk-test", account));
        crate::models::remember(
            &base_url(LETRONNA, ""),
            &[
                serde_json::json!({"id": "qwen-perk-test", "object": "model", "owned_by": "Qwen"}),
                serde_json::json!({"id": "other-perk-test", "object": "model", "owned_by": "OpenAI"}),
            ],
        );
        assert!(!needs_subscription(LETRONNA, "qwen-perk-test", account));
        assert!(needs_subscription(LETRONNA, "other-perk-test", account));
    }

    #[test]
    fn a_new_path_keeps_the_host_and_a_new_host_does_not() {
        assert!(same_host(
            "http://localhost:1234/v1",
            "http://LOCALHOST:1234/v2"
        ));
        assert!(same_host("https://a.example/v1", "https://a.example:443/x"));
        assert!(!same_host("https://a.example/v1", "https://b.example/v1"));
        assert!(!same_host(
            "http://localhost:1234/v1",
            "http://localhost:8080/v1"
        ));
        assert!(!same_host("http://a.example/v1", "https://a.example/v1"));
    }
}
