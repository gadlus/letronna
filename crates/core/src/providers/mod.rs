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
mod letronna;
mod ollama;
mod openai;
mod openrouter;

#[cfg(test)]
mod _template;
#[cfg(test)]
mod suite;

pub use router::Quota;
pub use wire::Wire;

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
}

/// Every provider, in the order they are offered. Letronna leads: it is what
/// a new install starts on.
pub static PROVIDERS: &[&dyn Provider] = &[
    &letronna::Letronna,
    &openai::OpenAi,
    &chatgpt::ChatGpt,
    &anthropic::Anthropic,
    &openrouter::OpenRouter,
    &ollama::Ollama,
    &custom::Custom,
];

/// The provider called `name`.
pub fn find(name: &str) -> Option<&'static dyn Provider> {
    PROVIDERS.iter().copied().find(|p| p.info().name == name)
}
