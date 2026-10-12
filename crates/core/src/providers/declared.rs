//! Providers a plugin declares in its `plugin.json`: data only, an
//! OpenAI-compatible API at a base URL, never a program. One is offered
//! while its plugin is installed, on and allowed, beside the built-in ones,
//! and its key is kept in the vault under its name as theirs are.

use super::{AuthKind, PROVIDERS, Provider, ProviderInfo, Tier};
use serde::{Deserialize, Serialize};
use std::sync::Mutex;

/// The longest name a plugin may give a provider.
const NAME_CHARS: usize = 40;

/// One entry of a manifest's `providers`.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
pub(crate) struct Entry {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub base: String,
    #[serde(default)]
    pub blurb: String,
    #[serde(default)]
    pub docs: String,
    /// `subscription`, `api_key`, `credit` or `free`; `api_key` if left out.
    #[serde(default)]
    pub tier: String,
    /// `api_key`, `none` or `url`; `api_key` if left out.
    #[serde(default)]
    pub auth: String,
}

/// A provider read from a manifest. Its strings live for the process, as a
/// built-in one's do; the same entry read again is the same provider.
#[derive(Debug, PartialEq)]
struct Declared {
    name: &'static str,
    base: &'static str,
    blurb: &'static str,
    docs: &'static str,
    tier: Tier,
    auth: AuthKind,
}

impl Provider for Declared {
    fn info(&self) -> ProviderInfo {
        ProviderInfo {
            name: self.name,
            base: self.base,
            blurb: self.blurb,
            docs: self.docs,
            tier: self.tier,
        }
    }

    fn auth(&self) -> AuthKind {
        self.auth
    }
}

/// `entry` as a provider, or why it cannot be one.
fn checked(entry: &Entry) -> Result<(Tier, AuthKind), String> {
    let name = entry.name.trim();
    let named = |why: &str| format!("The provider \"{name}\" is left out: {why}");
    if name.is_empty()
        || name.chars().count() > NAME_CHARS
        || !name
            .chars()
            .all(|c| c.is_alphanumeric() || matches!(c, ' ' | '-' | '_' | '.'))
    {
        return Err(format!(
            "A provider is left out: a name is up to {NAME_CHARS} letters, digits, spaces, \
             - _ and ."
        ));
    }
    if PROVIDERS
        .iter()
        .any(|p| p.info().name.eq_ignore_ascii_case(name))
    {
        return Err(named("a built-in provider has that name."));
    }
    let base = entry.base.trim();
    let local = base.starts_with("http://127.0.0.1:") || base.starts_with("http://127.0.0.1/");
    if !(local || base.starts_with("https://")) || base.contains(char::is_whitespace) {
        return Err(named(
            "its base must be an https:// URL or one on http://127.0.0.1.",
        ));
    }
    let tier = match entry.tier.trim() {
        "" => Tier::ApiKey,
        tier => Tier::parse(tier)
            .ok_or_else(|| named("its tier is not subscription, api_key, credit or free."))?,
    };
    let auth = match entry.auth.trim() {
        "" | "api_key" => AuthKind::ApiKey,
        // Nothing to set up means it is used unasked, so only this machine.
        "none" if local => AuthKind::None,
        "none" => return Err(named("one that needs no key must be on http://127.0.0.1.")),
        "url" => AuthKind::Url,
        _ => return Err(named("its auth is not api_key, none or url.")),
    };
    Ok((tier, auth))
}

/// Why `entry` would be left out, if it would.
pub(crate) fn refused(entry: &Entry) -> Option<String> {
    checked(entry).err()
}

/// Every provider made so far, so reading a manifest again makes nothing new.
fn made() -> &'static Mutex<Vec<&'static Declared>> {
    static MADE: Mutex<Vec<&'static Declared>> = Mutex::new(Vec::new());
    &MADE
}

/// What the plugins in use declare now, as of the last `reload`.
fn current() -> &'static Mutex<Option<Vec<&'static Declared>>> {
    static CURRENT: Mutex<Option<Vec<&'static Declared>>> = Mutex::new(None);
    &CURRENT
}

fn leak(text: &str) -> &'static str {
    Box::leak(text.trim().to_owned().into_boxed_str())
}

fn provider(entry: &Entry) -> Option<&'static Declared> {
    let (tier, auth) = checked(entry).ok()?;
    let mut made = made().lock().ok()?;
    let same = |d: &&&Declared| {
        d.name == entry.name.trim()
            && d.base == entry.base.trim()
            && d.blurb == entry.blurb.trim()
            && d.docs == entry.docs.trim()
            && d.tier == tier
            && d.auth == auth
    };
    if let Some(found) = made.iter().find(same) {
        return Some(found);
    }
    // ponytail: a changed entry leaks its old strings, a few bytes per edit.
    let fresh: &'static Declared = Box::leak(Box::new(Declared {
        name: leak(&entry.name),
        base: leak(&entry.base),
        blurb: leak(&entry.blurb),
        docs: leak(&entry.docs),
        tier,
        auth,
    }));
    made.push(fresh);
    Some(fresh)
}

/// The providers of the plugins in use, read once and again after `reload`.
pub(crate) fn list() -> Vec<&'static dyn Provider> {
    let mut current = match current().lock() {
        Ok(current) => current,
        Err(_) => return Vec::new(),
    };
    let list = current.get_or_insert_with(|| {
        let mut list: Vec<&'static Declared> = Vec::new();
        for entry in crate::plugins::providers() {
            // The first plugin to give a name keeps it.
            if list
                .iter()
                .any(|d| d.name.eq_ignore_ascii_case(entry.name.trim()))
            {
                continue;
            }
            list.extend(provider(&entry));
        }
        list
    });
    list.iter().map(|d| *d as &dyn Provider).collect()
}

/// Read the plugins' providers afresh, after one is added, removed, switched
/// or allowed.
pub fn reload() {
    if let Ok(mut current) = current().lock() {
        *current = None;
    }
}
