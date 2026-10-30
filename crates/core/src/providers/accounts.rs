//! More than one account of a provider: a second ChatGPT plan, a Claude
//! subscription for work beside a personal one, another OpenAI key.
//!
//! The first account of a provider is the provider itself, under its own
//! name, as it always was, so nothing saved before there were accounts --
//! its key or sign-in, a session's gateway, a fallback -- changes. Each
//! further account is a gateway of its own called "<Provider> · <label>",
//! listed beside the provider and resolved as it is: its key or sign-in is
//! kept under that name, and requests say which account they are on
//! (`chat::Connection::gateway`), so the router can move a request to the
//! next account of the same provider when one reaches its limit.
//!
//! The accounts are kept in `accounts.json`, in the order the user put
//! them, which is the order they are reached for. A line with no label
//! marks where the provider's own first account stands in that order.

use super::Provider;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};
use std::time::SystemTime;

/// What separates a provider's name from an account's label: never in a
/// gateway's own name, so an account's name cannot be mistaken for one.
const SEPARATOR: &str = " · ";

/// The most characters a label may have: room for "Personal" or "Client
/// work", and short enough for the model menu.
const LABEL_CHARS: usize = 24;

/// An account beyond the first, as the lists and the router know it.
#[derive(Clone, Debug, PartialEq)]
pub struct Account {
    /// What settings, sessions and its key or sign-in call it.
    pub name: &'static str,
    /// The provider it is an account of, by name.
    pub provider: &'static str,
    pub label: String,
}

/// A line of accounts.json.
#[derive(Clone, Serialize, Deserialize)]
struct Kept {
    provider: String,
    /// Empty for the provider's own first account.
    #[serde(default)]
    label: String,
}

fn path() -> PathBuf {
    crate::chat::data_dir().join("accounts.json")
}

fn read_kept(path: &Path) -> Vec<Kept> {
    std::fs::read(path)
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

fn write_kept(kept: &[Kept]) -> Result<(), String> {
    let _ = std::fs::create_dir_all(crate::chat::data_dir());
    let bytes = serde_json::to_vec_pretty(kept).map_err(|e| e.to_string())?;
    std::fs::write(path(), bytes).map_err(|e| format!("Cannot save the accounts: {e}"))?;
    super::router::forget();
    Ok(())
}

/// The file as it is, read again only once it has changed: every list of
/// gateways asks, and the window and the terminal each write it.
fn kept() -> Vec<Kept> {
    type Seen = (PathBuf, Option<(SystemTime, u64)>, Vec<Kept>);
    static SEEN: Mutex<Option<Seen>> = Mutex::new(None);
    let path = path();
    let stamp = std::fs::metadata(&path)
        .ok()
        .and_then(|meta| Some((meta.modified().ok()?, meta.len())));
    let mut seen = SEEN.lock().unwrap_or_else(PoisonError::into_inner);
    if let Some((at, was, list)) = seen.as_ref()
        && *at == path
        && *was == stamp
        // A write within the clock's step keeps the old time.
        && stamp.is_some()
    {
        return list.clone();
    }
    let list = read_kept(&path);
    *seen = Some((path, stamp, list.clone()));
    list
}

/// The built-in provider called `name`, as accounts are only ever of one.
fn built_in(name: &str) -> Option<&'static dyn Provider> {
    super::PROVIDERS
        .iter()
        .copied()
        .find(|p| p.info().name == name)
}

/// Whether a provider takes more than one account: one paid for with a key
/// or a sign-in. Letronna's own account, a server on this computer and
/// Custom's address do not; a second address is a gateway of the user's own.
pub fn takes_more(provider: &str) -> bool {
    built_in(provider).is_some_and(|p| {
        matches!(
            p.auth(),
            super::AuthKind::ApiKey | super::AuthKind::OAuth(_)
        )
    })
}

/// Every account beyond a provider's first, in the order kept.
pub fn list() -> Vec<Account> {
    let mut list: Vec<Account> = Vec::new();
    for line in kept() {
        let label = line.label.trim();
        let Some(provider) = built_in(line.provider.trim()).map(|p| p.info().name) else {
            continue;
        };
        if label.is_empty() || !takes_more(provider) {
            continue;
        }
        let name = interned(&format!("{provider}{SEPARATOR}{label}"));
        // A file edited by hand can name one twice.
        if list.iter().any(|a| a.name.eq_ignore_ascii_case(name)) {
            continue;
        }
        list.push(Account {
            name,
            provider,
            label: label.to_owned(),
        });
    }
    list
}

/// The account called `name`, if it is one beyond a provider's first.
pub fn get(name: &str) -> Option<Account> {
    list().into_iter().find(|a| a.name == name)
}

/// The provider `name` is an account of: itself for a provider's own first
/// account, its provider for one beyond it.
pub fn provider_of(name: &str) -> Option<&'static str> {
    built_in(name)
        .map(|p| p.info().name)
        .or_else(|| get(name).map(|a| a.provider))
}

/// The provider an account beyond the first is of.
pub(crate) fn find(name: &str) -> Option<&'static dyn Provider> {
    get(name).and_then(|a| built_in(a.provider))
}

/// Every account of `provider`, its own first one included, in the order
/// they are reached for.
pub fn of(provider: &str) -> Vec<&'static str> {
    let Some(own) = built_in(provider).map(|p| p.info().name) else {
        return Vec::new();
    };
    let extra = list();
    let mut names = Vec::new();
    let mut placed = false;
    for line in kept() {
        if line.provider.trim() != own {
            continue;
        }
        let label = line.label.trim();
        if label.is_empty() {
            if !placed {
                names.push(own);
                placed = true;
            }
        } else if let Some(account) = extra.iter().find(|a| a.provider == own && a.label == label)
            && !names.contains(&account.name)
        {
            names.push(account.name);
        }
    }
    if !placed {
        names.insert(0, own);
    }
    names
}

/// Whether `a` and `b` are accounts of the same provider.
pub fn same_provider(a: &str, b: &str) -> bool {
    matches!((provider_of(a), provider_of(b)), (Some(x), Some(y)) if x == y)
}

/// What a new account of `provider` labelled `label` would be called, or why
/// it cannot be.
fn check(provider: &str, label: &str) -> Result<String, String> {
    if !takes_more(provider) {
        return Err(format!("{provider} takes one account only."));
    }
    let label = label.trim();
    if label.is_empty() {
        return Err("Give the account a name, such as Work.".into());
    }
    if label.chars().count() > LABEL_CHARS {
        return Err(format!(
            "An account's name is {LABEL_CHARS} characters at most."
        ));
    }
    if label
        .chars()
        .any(|c| c.is_control() || matches!(c, ':' | '·'))
    {
        return Err("An account's name cannot hold a colon or a dot in the middle (·).".into());
    }
    let name = format!("{provider}{SEPARATOR}{label}");
    if let Some(holder) = crate::gateway::holder(&name) {
        return Err(format!(
            "There is an account called {holder} already: pick another name."
        ));
    }
    Ok(name)
}

/// A label for the next account of `provider` that none has: "Account 2",
/// "Account 3" and on.
pub fn next_label(provider: &str) -> String {
    (2..1000)
        .map(|n| format!("Account {n}"))
        .find(|label| check(provider, label).is_ok())
        .unwrap_or_else(|| "Another".into())
}

/// Add an account of `provider` labelled `label`, last in its order: its
/// name, under which its key is then saved or its sign-in kept.
pub fn add(provider: &str, label: &str) -> Result<&'static str, String> {
    let provider = built_in(provider.trim())
        .map(|p| p.info().name)
        .ok_or_else(|| format!("There is no provider called {provider}."))?;
    let name = check(provider, label)?;
    let mut lines = read_kept(&path());
    lines.push(Kept {
        provider: provider.to_owned(),
        label: label.trim().to_owned(),
    });
    write_kept(&lines)?;
    Ok(interned(&name))
}

/// Forget the account `name`, beyond its provider's first, with its key or
/// sign-in. A session left on it goes back to the default gateway, as one on
/// a gateway removed does.
pub fn remove(name: &str) -> Result<(), String> {
    let Some(account) = get(name) else {
        return Err(format!("There is no account called {name} to remove."));
    };
    let mut lines = read_kept(&path());
    lines.retain(|line| {
        !(line.provider.trim() == account.provider && line.label.trim() == account.label)
    });
    write_kept(&lines)?;
    let _ = super::auth::sign_out(name);
    super::usage::forget(name);
    crate::store::save_key(&key_slot_of(name), None)
        .map(|_| ())
        .map_err(|e| format!("The account is gone, but its key could not be removed: {e}."))
}

/// Move the account `name` one place earlier (`up`) or later among its
/// provider's, its provider's own first account included.
pub fn shift(name: &str, up: bool) -> Result<(), String> {
    let provider =
        provider_of(name).ok_or_else(|| format!("There is no account called {name}."))?;
    let mut order = of(provider);
    let Some(at) = order.iter().position(|n| *n == name) else {
        return Ok(());
    };
    let to = match up {
        true if at > 0 => at - 1,
        false if at + 1 < order.len() => at + 1,
        _ => return Ok(()),
    };
    order.swap(at, to);
    // The lines of other providers stay where they were; this provider's
    // are written again in the new order, its own first account marked.
    let mut lines: Vec<Kept> = (read_kept(&path()).into_iter())
        .filter(|line| line.provider.trim() != provider)
        .collect();
    lines.extend(order.iter().map(|n| Kept {
        provider: provider.to_owned(),
        label: get(n).map(|a| a.label).unwrap_or_default(),
    }));
    write_kept(&lines)
}

/// Where an account's key is saved: `gateway:` and its name, as a gateway
/// of the user's own is, which no search service or provider is called.
pub(crate) fn key_slot_of(name: &str) -> String {
    format!("gateway:{name}")
}

/// `name` for as long as the process runs, kept once however often the
/// file is read, as the gateways' names are.
fn interned(name: &str) -> &'static str {
    use std::collections::HashSet;
    use std::sync::OnceLock;
    static NAMES: OnceLock<Mutex<HashSet<&'static str>>> = OnceLock::new();
    let mut names = NAMES
        .get_or_init(Default::default)
        .lock()
        .unwrap_or_else(PoisonError::into_inner);
    if let Some(kept) = names.get(name) {
        return kept;
    }
    let kept: &'static str = Box::leak(name.to_owned().into_boxed_str());
    names.insert(kept);
    kept
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_second_account_is_a_gateway_of_its_own_beside_its_provider() {
        let _held = super::super::auth::tests::vault("accounts-add");
        assert_eq!(of("ChatGPT"), ["ChatGPT"]);
        let work = add("ChatGPT", "Work").unwrap();
        assert_eq!(work, "ChatGPT · Work");
        assert_eq!(provider_of(work), Some("ChatGPT"));
        assert_eq!(of("ChatGPT"), ["ChatGPT", "ChatGPT · Work"]);
        assert!(same_provider("ChatGPT", work));
        assert!(!same_provider("Claude", work));
        // Resolved as its provider is: its sign-in, its address, its tier.
        assert_eq!(super::super::find(work).unwrap().info().name, "ChatGPT");
        let listed: Vec<&str> = (crate::gateway::gateways().into_iter())
            .map(|g| g.name)
            .collect();
        let at = listed.iter().position(|n| *n == "ChatGPT").unwrap();
        assert_eq!(listed[at + 1], work);
        assert_eq!(
            crate::gateway::base_url(work, ""),
            crate::gateway::base_url("ChatGPT", "")
        );
        // The same name twice, in any case, is refused.
        assert!(add("ChatGPT", "work").is_err());
        assert!(add("ChatGPT", "a:b").is_err());
        assert!(add("Ollama", "Second").is_err());
        assert_eq!(next_label("ChatGPT"), "Account 2");
    }

    #[test]
    fn the_order_of_a_providers_accounts_is_the_users_and_removing_one_forgets_its_key() {
        let _held = super::super::auth::tests::vault("accounts-order");
        let work = add("OpenAI", "Work").unwrap();
        let side = add("OpenAI", "Side").unwrap();
        assert_eq!(of("OpenAI"), ["OpenAI", work, side]);
        shift("OpenAI", false).unwrap();
        assert_eq!(of("OpenAI"), [work, "OpenAI", side]);
        shift(side, true).unwrap();
        assert_eq!(of("OpenAI"), [work, side, "OpenAI"]);
        // Its key is its own, apart from the provider's.
        let slot = crate::gateway::key_slot(work);
        assert_eq!(slot, "gateway:OpenAI · Work");
        crate::store::save_key(slot, Some("sk-work")).unwrap();
        assert_eq!(
            crate::store::load_keys().get(slot).map(String::as_str),
            Some("sk-work")
        );
        remove(work).unwrap();
        assert_eq!(of("OpenAI"), [side, "OpenAI"]);
        assert!(!crate::store::load_keys().contains_key(slot));
    }
}
