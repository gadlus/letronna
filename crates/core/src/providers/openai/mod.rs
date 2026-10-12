//! OpenAI's own API. Its reasoning models are sent the Responses API at
//! request time (`wire::openai_responses`); the rest chat completions.

use super::{AuthKind, Provider, ProviderInfo, Tier};

pub struct OpenAi;

impl Provider for OpenAi {
    fn info(&self) -> ProviderInfo {
        ProviderInfo {
            name: "OpenAI",
            base: "https://api.openai.com/v1",
            blurb: "OpenAI directly. Needs an OpenAI API key.",
            docs: "https://platform.openai.com/api-keys",
            tier: Tier::ApiKey,
        }
    }

    fn auth(&self) -> AuthKind {
        AuthKind::ApiKey
    }
}
