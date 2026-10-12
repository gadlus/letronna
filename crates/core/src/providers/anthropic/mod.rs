//! Anthropic's own API, spoken in its Messages format (`wire::anthropic`).

use super::{AuthKind, Provider, ProviderInfo, Tier, Wire};

pub struct Anthropic;

impl Provider for Anthropic {
    fn info(&self) -> ProviderInfo {
        ProviderInfo {
            name: "Anthropic",
            base: "https://api.anthropic.com/v1",
            blurb: "Claude directly. Needs an Anthropic API key.",
            docs: "https://console.anthropic.com/settings/keys",
            tier: Tier::ApiKey,
        }
    }

    fn auth(&self) -> AuthKind {
        AuthKind::ApiKey
    }

    fn wire(&self) -> Wire {
        Wire::Anthropic
    }
}
