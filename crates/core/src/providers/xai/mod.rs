//! xAI's Grok models, on its OpenAI-shaped API.

use super::{AuthKind, Provider, ProviderInfo, Tier};

pub struct Xai;

impl Provider for Xai {
    fn info(&self) -> ProviderInfo {
        ProviderInfo {
            name: "xAI",
            base: "https://api.x.ai/v1",
            blurb: "Grok models. Needs an xAI API key.",
            docs: "https://console.x.ai",
            tier: Tier::ApiKey,
        }
    }

    fn auth(&self) -> AuthKind {
        AuthKind::ApiKey
    }
}
