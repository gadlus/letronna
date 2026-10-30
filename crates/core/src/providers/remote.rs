//! This computer's accounts as a paired phone sees and changes them, over
//! the link the phone already talks to the computer on.
//!
//! The phone is a client: the accounts, their keys and their sign-ins stay
//! here, in this computer's vault. So a key goes one way, from the phone to
//! the computer, and nothing here ever gives one back, nor a sign-in's
//! tokens: a list says what each account is and how much of its plan is
//! left, never what it is signed in with.
//!
//! A sign-in started from the phone is finished on the phone, with no
//! browser here: ChatGPT by a code typed on OpenAI's page, Claude by
//! pasting back the code its page shows (`auth::begin_away`). It runs on a
//! thread of its own, and the phone asks how it stands (`sign_in_status`),
//! since a request held open until it ends would hold up the link.

use super::{AuthKind, Tier, accounts, auth, usage};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};

/// Every account set up or ready to be, each provider's first one included,
/// and the providers another account can be added to.
pub fn list() -> Value {
    let keys = crate::store::load_keys();
    let settings = crate::store::current_settings();
    let custom = &settings.custom_endpoint;
    let now = crate::chat::now();
    let mut rows = Vec::new();
    for gateway in crate::gateway::gateways() {
        let name = gateway.name;
        let Some(provider) = super::find(name) else {
            continue;
        };
        if !super::offered(name) || !shown(provider.auth()) {
            continue;
        }
        let (_, good, set_up) = crate::gateway::status(name, &keys, custom, 0);
        let of = accounts::provider_of(name).unwrap_or(name);
        let further = accounts::get(name);
        let tokens = auth::tokens(name);
        let known = usage::get(name).filter(|_| set_up);
        let plan = (known.as_ref().and_then(|u| u.plan.clone()))
            .or_else(|| tokens.as_ref().map(|t| t.plan.clone()))
            .filter(|p| !p.is_empty());
        rows.push(json!({
            "name": name,
            "provider": of,
            "label": further.map(|a| a.label),
            "auth": auth_name(provider.auth()),
            "setUp": set_up,
            "working": good,
            "plan": plan,
            "email": tokens.map(|t| t.email).filter(|e| !e.is_empty()),
            "usage": known.map(|u| usage_json(&u, now)),
        }));
    }
    let providers: Vec<Value> = (super::PROVIDERS.iter())
        .filter(|p| super::offered(p.info().name) && shown(p.auth()))
        .map(|p| {
            let info = p.info();
            json!({
                "name": info.name,
                "blurb": info.blurb,
                "docs": info.docs,
                "auth": auth_name(p.auth()),
                "plan": info.tier == Tier::Subscription,
                "takesMore": accounts::takes_more(info.name),
                "signIn": sign_in_kind(p.auth()),
                "caveat": p.caveat(),
                "nextLabel": accounts::next_label(info.name),
            })
        })
        .collect();
    json!({
        "accounts": rows,
        "providers": providers,
        "switchAccounts": settings.switch_accounts,
    })
}

/// Whether a provider is one a phone sets up: one paid for with a key or a
/// sign-in. Letronna's own account is the phone's already, a server on
/// this computer needs nothing, and an address is typed at the computer.
fn shown(auth: AuthKind) -> bool {
    matches!(auth, AuthKind::ApiKey | AuthKind::OAuth(_))
}

fn auth_name(auth: AuthKind) -> &'static str {
    match auth {
        AuthKind::OAuth(_) => "sign-in",
        _ => "key",
    }
}

/// How a provider's sign-in is finished on a phone: by typing a code on its
/// page, or by pasting back the code its page shows; null for a key.
fn sign_in_kind(auth: AuthKind) -> Value {
    match auth {
        AuthKind::OAuth(spec) if spec.device.is_some() => "code".into(),
        AuthKind::OAuth(spec) if spec.manual_redirect.is_some() => "paste".into(),
        _ => Value::Null,
    }
}

fn usage_json(known: &usage::Usage, now: u64) -> Value {
    json!({
        "windows": (known.windows.iter()).map(|w| json!({
            "label": w.label,
            "usedPercent": w.used_percent,
            "resetsAt": w.resets_at,
            "full": w.full(now),
        })).collect::<Vec<_>>(),
        "note": known.note,
        "checkedAt": known.checked_at,
    })
}

/// The account a phone's request means: `provider`'s first when it is not
/// set up yet, or a new one labelled `label` beside it when it is.
fn account_for(provider: &str, label: Option<&str>) -> Result<&'static str, String> {
    let first = super::PROVIDERS
        .iter()
        .map(|p| p.info().name)
        .find(|name| *name == provider)
        .ok_or_else(|| format!("There is no provider called {provider}."))?;
    let keys = crate::store::load_keys();
    let custom = crate::store::current_settings().custom_endpoint;
    let (_, _, set_up) = crate::gateway::status(first, &keys, &custom, 0);
    match label.map(str::trim).filter(|l| !l.is_empty()) {
        Some(label) if set_up => accounts::add(first, label),
        None if set_up => accounts::add(first, &accounts::next_label(first)),
        _ => Ok(first),
    }
}

/// Save `key` for an account of `provider` and ask it for its models: how
/// many it serves, or why the key did not work, in which case nothing is
/// kept.
pub fn add_key(provider: &str, label: Option<&str>, key: &str) -> Result<Value, String> {
    let key = key.trim();
    if key.is_empty() {
        return Err("Paste the key first.".into());
    }
    let name = account_for(provider, label)?;
    let added = accounts::get(name).is_some();
    let slot = crate::gateway::key_slot(name).to_owned();
    let keys = crate::store::save_key(&slot, Some(key)).map_err(|e| e.to_string())?;
    let custom = crate::store::current_settings().custom_endpoint;
    match crate::gateway::models_of(name, &keys, &custom) {
        Ok(models) => Ok(json!({"name": name, "models": models.len()})),
        Err(why) => {
            // A key that does not work is not left behind.
            let _ = crate::store::save_key(&slot, None);
            if added {
                let _ = accounts::remove(name);
            }
            Err(format!("{name} did not take the key: {why}"))
        }
    }
}

/// How a sign-in started from the phone stands.
#[derive(Clone)]
enum Standing {
    Waiting,
    Done,
    Failed(String),
}

struct Started {
    flow: Arc<auth::Away>,
    standing: Arc<Mutex<Standing>>,
    /// An account added for it, removed again if it is never signed in.
    added: bool,
}

fn started() -> &'static Mutex<HashMap<String, Started>> {
    static STARTED: std::sync::OnceLock<Mutex<HashMap<String, Started>>> =
        std::sync::OnceLock::new();
    STARTED.get_or_init(Default::default)
}

/// Start signing in to an account of `provider` on the phone: the page to
/// open there, and the code to type on it or none when its page shows one
/// to paste back. An id names it to `sign_in_paste`, `sign_in_status` and
/// `sign_in_cancel`.
pub fn sign_in_start(provider: &str, label: Option<&str>) -> Result<Value, String> {
    let name = account_for(provider, label)?;
    let added = accounts::get(name).is_some();
    let flow = match auth::begin_away(name) {
        Ok(flow) => flow,
        Err(why) => {
            if added {
                let _ = accounts::remove(name);
            }
            return Err(why);
        }
    };
    let id = auth::random(12)?;
    let standing = Arc::new(Mutex::new(Standing::Waiting));
    // A code typed on the provider's page is waited for at once; a pasted
    // one once it is pasted, as `finish` waits for the paste.
    {
        let (flow, standing) = (flow.clone(), standing.clone());
        std::thread::spawn(move || {
            let ended = match flow.finish() {
                Ok(_) => Standing::Done,
                Err(why) => Standing::Failed(why),
            };
            *standing.lock().unwrap_or_else(PoisonError::into_inner) = ended;
            super::router::forget();
        });
    }
    let reply = json!({
        "id": id,
        "name": name,
        "url": flow.url,
        "code": flow.code,
    });
    started()
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .insert(
            id,
            Started {
                flow,
                standing,
                added,
            },
        );
    Ok(reply)
}

/// Hand the sign-in `id` the code its page showed.
pub fn sign_in_paste(id: &str, code: &str) -> Result<Value, String> {
    let started = started().lock().unwrap_or_else(PoisonError::into_inner);
    let held = started
        .get(id)
        .ok_or("That sign-in has ended; start it again.")?;
    held.flow.paste(code)?;
    Ok(json!({"status": "waiting"}))
}

/// How the sign-in `id` stands: waiting, done, or failed and why. One that
/// ended is forgotten once it was asked about.
pub fn sign_in_status(id: &str) -> Value {
    let mut started = started().lock().unwrap_or_else(PoisonError::into_inner);
    let Some(held) = started.get(id) else {
        return json!({"status": "failed", "error": "That sign-in has ended; start it again."});
    };
    let standing = held
        .standing
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .clone();
    let name = held.flow.name;
    match standing {
        Standing::Waiting => json!({"status": "waiting", "name": name}),
        Standing::Done => {
            started.remove(id);
            json!({"status": "done", "name": name})
        }
        Standing::Failed(why) => {
            if let Some(held) = started.remove(id)
                && held.added
            {
                let _ = accounts::remove(name);
            }
            json!({"status": "failed", "name": name, "error": why})
        }
    }
}

/// Stop waiting for the sign-in `id`.
pub fn sign_in_cancel(id: &str) {
    if let Some(held) = started()
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .get(id)
    {
        held.flow.cancel();
    }
}

/// Sign out of `name`, or take its key off; a further account goes
/// altogether.
pub fn remove(name: &str) -> Result<(), String> {
    if accounts::get(name).is_some() {
        return accounts::remove(name);
    }
    match super::find(name).map(|p| p.auth()) {
        Some(AuthKind::OAuth(_)) => auth::sign_out(name),
        Some(AuthKind::ApiKey) => crate::store::save_key(crate::gateway::key_slot(name), None)
            .map(|_| ())
            .map_err(|e| e.to_string()),
        _ => Err(format!("{name} is not set up from the phone.")),
    }
    .map(|()| usage::forget(name))
}

/// Move `name` one place earlier, or later, among its provider's accounts.
pub fn shift(name: &str, up: bool) -> Result<(), String> {
    accounts::shift(name, up)
}

/// Go on with the next account of the same provider when one reaches its
/// limit, or not.
pub fn set_switch(on: bool) -> Result<(), String> {
    // Only this setting, so whatever the window or the terminal changed
    // meanwhile stays.
    crate::state::update::<crate::store::Settings>(&crate::store::settings_path(), |disk| {
        if !disk.is_object() {
            *disk = serde_json::to_value(crate::store::Settings::default()).unwrap_or_default();
        }
        disk["switch_accounts"] = on.into();
    })
}

/// How much is left of each plan set up, asked of the plans that have a
/// page for it, or of `name` alone; `asked` when the user asked again.
pub fn check_usage(name: Option<&str>, asked: bool) -> Value {
    let keys = crate::store::load_keys();
    let custom = crate::store::current_settings().custom_endpoint;
    let now = crate::chat::now();
    let names: Vec<&'static str> = (crate::gateway::ready(&keys, &custom).into_iter())
        .map(|(n, ..)| n)
        .filter(|n| name.is_none_or(|wanted| wanted == *n))
        .filter(|n| super::find(n).is_some_and(|p| p.info().tier == Tier::Subscription))
        .collect();
    let checked: serde_json::Map<String, Value> = (names.into_iter())
        .map(|n| {
            let value = match usage::check(n, asked) {
                Ok(known) => usage_json(&known, now),
                Err(why) => json!({"error": why}),
            };
            (n.to_owned(), value)
        })
        .collect();
    Value::Object(checked)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_list_names_each_account_and_its_usage_and_never_its_key() {
        let _held = super::super::auth::tests::vault("remote-list");
        crate::store::save_key("OpenAI", Some("sk-secret-openai")).unwrap();
        let work = accounts::add("OpenAI", "Work").unwrap();
        crate::store::save_key(crate::gateway::key_slot(work), Some("sk-secret-work")).unwrap();
        let listed = list();
        let text = listed.to_string();
        assert!(!text.contains("sk-secret"), "{text}");
        let names: Vec<&str> = (listed["accounts"].as_array().unwrap().iter())
            .filter_map(|a| a["name"].as_str())
            .collect();
        let at = names.iter().position(|n| *n == "OpenAI").unwrap();
        assert_eq!(names[at + 1], work);
        let row = &listed["accounts"][at + 1];
        assert_eq!(row["provider"], "OpenAI");
        assert_eq!(row["label"], "Work");
        assert_eq!(row["setUp"], true);
        // Letronna's own account is the phone's already; a local server
        // needs nothing.
        assert!(!names.contains(&"Letronna") && !names.contains(&"Ollama"));
        let chatgpt = (listed["providers"].as_array().unwrap().iter())
            .find(|p| p["name"] == "OpenCode Go/Zen")
            .unwrap();
        assert_eq!(chatgpt["auth"], "key");
        assert_eq!(chatgpt["plan"], true);
    }

    #[test]
    fn an_empty_key_or_unknown_provider_is_refused_and_the_switch_is_saved_alone() {
        let _held = super::super::auth::tests::vault("remote-key");
        // Nothing answers on the discard port.
        set_switch(false).unwrap();
        assert!(!crate::store::current_settings().switch_accounts);
        assert!(add_key("OpenAI", None, " ").is_err());
        assert!(add_key("Nobody", None, "k").is_err());
    }

    #[test]
    fn removing_a_further_account_takes_it_away_and_a_first_one_only_its_key() {
        let _held = super::super::auth::tests::vault("remote-remove");
        crate::store::save_key("xAI", Some("xai-key")).unwrap();
        let side = accounts::add("xAI", "Side").unwrap();
        remove(side).unwrap();
        assert!(accounts::get(side).is_none());
        remove("xAI").unwrap();
        assert!(!crate::store::load_keys().contains_key("xAI"));
    }
}
