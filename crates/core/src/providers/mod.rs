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
pub mod chatgpt;
pub mod claude;
mod custom;
pub(crate) mod declared;
mod gemini;
mod letronna;
mod ollama;
mod openai;
mod openrouter;
mod this_computer;
mod xai;

#[cfg(test)]
mod _template;
#[cfg(test)]
mod suite;

pub use declared::reload;
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
    &claude::Claude,
    &anthropic::Anthropic,
    &gemini::Gemini,
    &xai::Xai,
    &openrouter::OpenRouter,
    &this_computer::ThisComputer,
    &ollama::Ollama,
    &custom::Custom,
];

/// Every provider: the built-in ones, then those the plugins in use
/// declare in their manifests.
pub fn all() -> Vec<&'static dyn Provider> {
    let mut all = PROVIDERS.to_vec();
    all.extend(declared::list());
    all
}

/// The provider called `name`.
pub fn find(name: &str) -> Option<&'static dyn Provider> {
    (PROVIDERS.iter().copied())
        .find(|p| p.info().name == name)
        .or_else(|| declared::list().into_iter().find(|p| p.info().name == name))
}

/// Whether the provider called `name` is offered now: always, unless a
/// plugin offers it, and then only while that plugin is on.
pub fn offered(name: &str) -> bool {
    find(name).and_then(|p| p.plugin()).is_none_or(plugged)
}

/// The window's plugin switches, by id, in their own file: `plugins.json`
/// holds what the marketplace keeps.
const SWITCHES: &str = "plugin-switches.json";

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

/// The switches as last read: the file, when it changed, and what it held.
type Seen = Option<(
    std::path::PathBuf,
    std::time::SystemTime,
    HashMap<String, bool>,
)>;
static SEEN: Mutex<Seen> = Mutex::new(None);

/// The switches as saved, read again only when the file has changed, so a
/// listing that asks once per provider reads it at most once.
pub fn switches() -> HashMap<String, bool> {
    let path = crate::chat::data_dir().join(SWITCHES);
    let stamp = std::fs::metadata(&path).and_then(|m| m.modified()).ok();
    let Ok(mut seen) = SEEN.lock() else {
        return HashMap::new();
    };
    if let (Some((at_path, at, saved)), Some(stamp)) = (&*seen, stamp)
        && *at_path == path
        && *at == stamp
    {
        return saved.clone();
    }
    // Before they had a file of their own the switches were the bools at the
    // top of `plugins.json`, beside the marketplace's records.
    let read = |file: &std::path::Path| {
        let bytes = std::fs::read(file).ok()?;
        let map: serde_json::Map<String, serde_json::Value> =
            serde_json::from_slice(&bytes).ok()?;
        Some(
            (map.into_iter())
                .filter_map(|(id, on)| Some((id, on.as_bool()?)))
                .collect::<HashMap<_, _>>(),
        )
    };
    let saved = match stamp {
        Some(_) => read(&path),
        None => read(&crate::chat::data_dir().join("plugins.json")),
    }
    .unwrap_or_default();
    *seen = stamp.map(|stamp| (path, stamp, saved.clone()));
    saved
}

/// Save the switch `id` as `on`, keeping the others.
pub fn save_switch(id: &str, on: bool) {
    let mut saved = switches();
    saved.insert(id.to_owned(), on);
    let dir = crate::chat::data_dir();
    let _ = std::fs::create_dir_all(&dir).and_then(|_| {
        std::fs::write(
            dir.join(SWITCHES),
            serde_json::to_vec_pretty(&saved).unwrap_or_default(),
        )
    });
    // A write within the clock's step keeps the old time.
    if let Ok(mut seen) = SEEN.lock() {
        *seen = None;
    }
}

/// Whether the plugin `id` starts on before it was ever switched: off,
/// unless it offers a provider already signed in to, as ChatGPT was before
/// it became a plugin, so an upgrade keeps its models.
pub fn starts_on(id: &str) -> bool {
    PROVIDERS
        .iter()
        .filter(|p| p.plugin() == Some(id))
        .any(|p| auth::signed_in(p.info().name))
}

/// Whether the plugin `id` is on: as the window said, or, in a process with
/// no plugins of its own such as the terminal, as the window saved it.
fn plugged(id: &str) -> bool {
    if let Some(on) = switched().lock().ok().and_then(|s| s.get(id).copied()) {
        return on;
    }
    switches().get(id).copied().unwrap_or_else(|| starts_on(id))
}

#[cfg(test)]
pub(crate) mod tests {
    /// Switch the plugin `id` on as the window saves it, in the data folder
    /// the test holds.
    pub(crate) fn plugged_in(id: &str) {
        super::save_switch(id, true);
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

    #[test]
    fn chatgpt_starts_on_once_for_whoever_was_signed_in_before_it_was_a_plugin() {
        let _held = super::auth::tests::vault("plugged-migrate");
        assert!(!super::plugged("chatgpt"));
        super::auth::tests::keep("ChatGPT", "access-1");
        assert!(super::starts_on("chatgpt"));
        assert!(super::plugged("chatgpt"));
        // Once switched off it stays off, signed in or not.
        super::save_switch("chatgpt", false);
        assert!(!super::plugged("chatgpt"));
    }

    #[test]
    fn a_claude_subscription_is_offered_only_while_its_plugin_is_on() {
        let _held = super::auth::tests::vault("plugged-claude");
        assert!(!super::offered("Claude"));
        plugged_in("claude");
        assert!(super::offered("Claude"));
    }

    #[test]
    fn the_switches_keep_out_of_the_marketplaces_records() {
        let _held = super::auth::tests::vault("plugged-apart");
        let market = crate::chat::data_dir().join("plugins.json");
        // As the window saved them before they had a file of their own.
        let before = r#"{"plugins":{"notes":{"source":"https://example.com/notes.git"}},"markets":["https://example.com/index.json"],"discord-presence":false}"#;
        std::fs::write(&market, before).unwrap();
        assert_eq!(super::switches().get("discord-presence"), Some(&false));
        super::save_switch("chatgpt", true);
        super::save_switch("discord-presence", true);
        assert_eq!(std::fs::read_to_string(&market).unwrap(), before);
        let saved = super::switches();
        assert_eq!((saved["chatgpt"], saved["discord-presence"]), (true, true));
    }
}
