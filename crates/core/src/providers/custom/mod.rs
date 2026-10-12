//! Any OpenAI-compatible server, at the base URL the user gives.

use super::{AuthKind, Provider, ProviderInfo, Tier};

pub struct Custom;

impl Provider for Custom {
    fn info(&self) -> ProviderInfo {
        ProviderInfo {
            name: "Custom",
            base: "",
            blurb: "Any OpenAI-compatible server. Paste the base URL.",
            docs: "https://letronna.ai/docs/gateways",
            tier: Tier::ApiKey,
        }
    }

    fn auth(&self) -> AuthKind {
        AuthKind::Url
    }
}
