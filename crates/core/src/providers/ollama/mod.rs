//! Ollama on this machine. Nothing to sign in to; it is ready whenever it
//! is running.

use super::{AuthKind, Provider, ProviderInfo, Tier};

pub struct Ollama;

impl Provider for Ollama {
    fn info(&self) -> ProviderInfo {
        ProviderInfo {
            name: "Ollama",
            base: "http://localhost:11434/v1",
            blurb: "Local models on this machine. Works offline.",
            docs: "https://ollama.com/download",
            tier: Tier::Free,
        }
    }

    fn auth(&self) -> AuthKind {
        AuthKind::None
    }
}
