//! The Codex models a ChatGPT plan includes, signed in with ChatGPT: core's
//! ChatGPT provider, offered only while this plugin is on. The sign-in,
//! requests and limits are all the provider's; the plugin is its switch, and
//! the Plugins page shows the provider's account row under it.

use super::{AppEvent, Plugin};
use crate::prelude::*;
use std::sync::atomic::{AtomicBool, Ordering};

/// The provider it offers.
const PROVIDER: &str = "ChatGPT";

pub(crate) struct ChatGpt {
    enabled: AtomicBool,
}

impl ChatGpt {
    pub(crate) fn new() -> Self {
        Self {
            enabled: AtomicBool::new(false),
        }
    }
}

impl Plugin for ChatGpt {
    fn id(&self) -> &'static str {
        "chatgpt"
    }
    fn name(&self) -> &'static str {
        "ChatGPT"
    }
    fn description(&self) -> &'static str {
        "The Codex models your ChatGPT plan includes, signed in with ChatGPT instead of an API key."
    }

    fn init(&self, _cx: &mut App) {}

    fn handle(&self, _event: &AppEvent) {}

    fn set_enabled(&self, enabled: bool) {
        self.enabled.store(enabled, Ordering::Relaxed);
        letronna_core::providers::plug(self.id(), enabled);
    }
    fn enabled(&self) -> bool {
        self.enabled.load(Ordering::Relaxed)
    }

    // Opt-in: it signs in with a plan rather than a key.
    fn default_enabled(&self) -> bool {
        false
    }

    fn provider(&self) -> Option<&'static str> {
        Some(PROVIDER)
    }
}

#[cfg(test)]
mod tests {
    use super::{ChatGpt, PROVIDER, Plugin};

    #[test]
    fn its_id_is_the_one_the_provider_names() {
        let provider = letronna_core::providers::find(PROVIDER).unwrap();
        assert_eq!(provider.plugin(), Some(ChatGpt::new().id()));
    }
}
