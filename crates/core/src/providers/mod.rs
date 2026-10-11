//! The services Letronna sends model requests to, one folder each.
//!
//! A provider says who it is, how it is signed in to and which wire format
//! it speaks; everything shared -- building requests, reading replies,
//! keeping keys -- lives outside its folder, so adding one is a folder and a
//! line in `PROVIDERS`. `_template/` is the folder to copy, and
//! `docs/providers.md` walks through it. Every provider in the list passes
//! the same checks (`suite.rs`), against recorded replies rather than real
//! keys.
//!
//! A provider is named by its `name`: settings, saved keys and fallbacks
//! all refer to it that way, so a name never changes once shipped.

pub mod auth;
pub mod local;
pub mod router;
pub mod wire;

mod anthropic;
mod chatgpt;
mod custom;
mod gemini;
mod letronna;
mod ollama;
mod openai;
mod openrouter;
mod xai;

#[cfg(test)]
mod _template;
#[cfg(test)]
mod suite;

pub use router::Quota;
pub use wire::Wire;

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

/// Who a provider is, as the gateway list and `/gateways` show it.
pub struct ProviderInfo {
    pub name: &'static str,
    /// Where its OpenAI-shaped API is, ending in the version (`.../v1`).
    /// Empty when the user gives it, as for Custom.
    pub base: &'static str,
    /// One line on what it is and what it needs.
    pub blurb: &'static str,
    /// Where to read about getting a key or an account.
    pub docs: &'static str,
    pub tier: Tier,
}

/// What a provider costs to use, in the order a fallback reaches for them:
/// a plan already paid for first, a free one last.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Tier {
    Subscription,
    ApiKey,
    Credit,
    Free,
}

impl Tier {
    pub const ALL: [Tier; 4] = [Tier::Subscription, Tier::ApiKey, Tier::Credit, Tier::Free];

    /// How Settings writes it in `provider_order`.
    pub fn name(self) -> &'static str {
        match self {
            Tier::Subscription => "subscription",
            Tier::ApiKey => "api_key",
            Tier::Credit => "credit",
            Tier::Free => "free",
        }
    }

    pub fn parse(name: &str) -> Option<Tier> {
        Tier::ALL.into_iter().find(|t| t.name() == name.trim())
    }
}

/// What a provider needs before it can be asked.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AuthKind {
    /// Nothing: it runs on this machine.
    None,
    /// An API key the user pastes, saved in the vault under the name.
    ApiKey,
    /// This computer's Letronna sign-in.
    Account,
    /// A base URL the user gives, and a key if that server wants one.
    Url,
    /// A sign-in in the browser, as `spec` describes it (`auth/`).
    OAuth(&'static auth::OAuthSpec),
}

pub trait Provider: Send + Sync {
    fn info(&self) -> ProviderInfo;

    fn auth(&self) -> AuthKind;

    /// The format its base URL speaks. A model may still be sent another one
    /// at request time, as OpenAI's reasoning models are sent Responses.
    fn wire(&self) -> Wire {
        Wire::OpenAiChat
    }

    /// The model ids it serves at `base`, asked with `key`.
    fn models(&self, base: &str, key: &str) -> Result<Vec<String>, String> {
        crate::chat::list_models(base, key)
    }

    /// How much of its limits the account has used, read off a reply's
    /// headers: the standard rate-limit ones unless it says otherwise.
    fn quota(&self, headers: &reqwest::header::HeaderMap) -> Option<Quota> {
        router::rate_limits(headers, crate::chat::now())
    }

    /// What the user should know before connecting it, shown beside its
    /// sign-in where there is something.
    fn caveat(&self) -> Option<&'static str> {
        None
    }

    /// The id of the window plugin that offers it, for a provider that is
    /// there only while that plugin is on; `None` for one always offered.
    fn plugin(&self) -> Option<&'static str> {
        None
    }
}

/// Every provider, in the order they are offered. Letronna leads: it is what
/// a new install starts on.
pub static PROVIDERS: &[&dyn Provider] = &[
    &letronna::Letronna,
    &openai::OpenAi,
    &chatgpt::ChatGpt,
    &anthropic::Anthropic,
    &gemini::Gemini,
    &xai::Xai,
    &openrouter::OpenRouter,
    &ollama::Ollama,
    &custom::Custom,
];

/// The provider called `name`.
pub fn find(name: &str) -> Option<&'static dyn Provider> {
    PROVIDERS.iter().copied().find(|p| p.info().name == name)
}

/// Whether the provider called `name` is offered now: always, unless a
/// plugin offers it, and then only while that plugin is on.
pub fn offered(name: &str) -> bool {
    find(name).and_then(|p| p.plugin()).is_none_or(plugged)
}

/// What the window said its plugins are, by id, once it has.
fn switched() -> &'static Mutex<HashMap<&'static str, bool>> {
    static SWITCHED: OnceLock<Mutex<HashMap<&'static str, bool>>> = OnceLock::new();
    SWITCHED.get_or_init(Default::default)
}

/// Turn the plugin `id` on or off for this process, as the window does as
/// it starts and when its switch is flipped.
pub fn plug(id: &'static str, on: bool) {
    if let Ok(mut switched) = switched().lock() {
        switched.insert(id, on);
    }
}

/// Whether the plugin `id` is on: as the window said, or, in a process with
/// no plugins of its own such as the terminal, as the window saved it in
/// `plugins.json`. Off until switched on.
fn plugged(id: &str) -> bool {
    if let Some(on) = switched().lock().ok().and_then(|s| s.get(id).copied()) {
        return on;
    }
    std::fs::read(crate::chat::data_dir().join("plugins.json"))
        .ok()
        .and_then(|bytes| serde_json::from_slice::<HashMap<String, bool>>(&bytes).ok())
        .and_then(|saved| saved.get(id).copied())
        .unwrap_or(false)
}

#[cfg(test)]
pub(crate) mod tests {
    /// Switch the plugin `id` on as the window saves it, in the data folder
    /// the test holds.
    pub(crate) fn plugged_in(id: &str) {
        let path = crate::chat::data_dir().join("plugins.json");
        std::fs::write(path, format!("{{\"{id}\": true}}")).unwrap();
    }

    #[test]
    fn a_plugin_is_off_until_switched_on_and_the_window_has_the_last_word() {
        let _held = super::auth::tests::vault("plugged");
        assert!(!super::plugged("plugged-test"));
        plugged_in("plugged-test");
        assert!(super::plugged("plugged-test"));
        // What the window says outranks what it saved.
        super::plug("plugged-test", false);
        assert!(!super::plugged("plugged-test"));
        // A provider no plugin offers is always there.
        assert!(super::offered("OpenAI"));
    }
}
