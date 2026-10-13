//! A Claude subscription, signed in with Claude: core's Claude provider,
//! offered only while this plugin is on. The sign-in, requests and limits are
//! all the provider's; the plugin is its switch, and the Plugins page shows
//! the provider's account row under it.

use super::{AppEvent, Plugin};
use crate::prelude::*;
use letronna_core::providers;
use std::sync::atomic::{AtomicBool, Ordering};

/// The provider it offers.
const PROVIDER: &str = "Claude";

pub(crate) struct Claude {
    enabled: AtomicBool,
}

impl Claude {
    pub(crate) fn new() -> Self {
        Self {
            enabled: AtomicBool::new(false),
        }
    }
}

impl Plugin for Claude {
    fn id(&self) -> &'static str {
        "claude"
    }
    fn name(&self) -> &'static str {
        "Claude"
    }
    fn description(&self) -> &'static str {
        "The models your Claude subscription includes, signed in with Claude instead of an API key. Unofficial: it signs in as Claude Code does, which Anthropic may not allow."
    }

    fn init(&self, _cx: &mut App) {}

    fn handle(&self, _event: &AppEvent) {}

    fn set_enabled(&self, enabled: bool) {
        self.enabled.store(enabled, Ordering::Relaxed);
        providers::plug(self.id(), enabled);
    }
    fn enabled(&self) -> bool {
        self.enabled.load(Ordering::Relaxed)
    }

    // Opt-in. One already signed in from before it was a plugin starts on,
    // so its models stay.
    fn default_enabled(&self) -> bool {
        providers::starts_on(self.id())
    }

    fn provider(&self) -> Option<&'static str> {
        Some(PROVIDER)
    }
}

#[cfg(test)]
mod tests {
    use super::{Claude, PROVIDER, Plugin};

    #[test]
    fn its_id_is_the_one_the_provider_names() {
        let provider = letronna_core::providers::find(PROVIDER).unwrap();
        assert_eq!(provider.plugin(), Some(Claude::new().id()));
    }
}
