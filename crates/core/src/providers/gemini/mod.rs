//! Google's Gemini models, through its OpenAI-compatible endpoint. Its
//! `/models` reply names each model `models/<id>`, while chat completions
//! take the bare id, so the prefix is dropped.

use super::{AuthKind, Provider, ProviderInfo, Tier};

pub struct Gemini;

impl Provider for Gemini {
    fn info(&self) -> ProviderInfo {
        ProviderInfo {
            name: "Gemini",
            base: "https://generativelanguage.googleapis.com/v1beta/openai",
            blurb: "Google's Gemini models. Needs a Gemini API key.",
            docs: "https://aistudio.google.com/apikey",
            tier: Tier::ApiKey,
        }
    }

    fn auth(&self) -> AuthKind {
        AuthKind::ApiKey
    }

    fn models(&self, base: &str, key: &str) -> Result<Vec<String>, String> {
        let ids = crate::chat::list_models(base, key)?;
        Ok(ids
            .into_iter()
            .map(|id| id.strip_prefix("models/").map(str::to_owned).unwrap_or(id))
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drops_the_models_prefix() {
        let base = super::super::suite::serve(include_str!("../fixtures/gemini_models.json"));
        let models = Gemini.models(&base, "test-key").unwrap();
        assert_eq!(models, ["gemini-test-flash", "gemini-test-pro"]);
    }
}
