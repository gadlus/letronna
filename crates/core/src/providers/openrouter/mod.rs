//! OpenRouter: many providers' models behind one key.

use super::{AuthKind, Provider, ProviderInfo, Tier};

pub struct OpenRouter;

impl Provider for OpenRouter {
    fn info(&self) -> ProviderInfo {
        ProviderInfo {
            name: "OpenRouter",
            base: "https://openrouter.ai/api/v1",
            blurb: "Hundreds of models behind one key.",
            docs: "https://openrouter.ai/keys",
            tier: Tier::ApiKey,
        }
    }

    fn auth(&self) -> AuthKind {
        AuthKind::ApiKey
    }
}
