//! The Codex models a ChatGPT plan includes, signed in with ChatGPT: core's
//! ChatGPT provider, offered only while this plugin is on. The sign-in,
//! requests and limits are all the provider's; the plugin is its switch, and
//! the Plugins page shows the provider's account row under it.

use super::{AppEvent, Plugin};
use crate::prelude::*;
use letronna_core::providers;
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

    // Opt-in: it signs in with a plan rather than a key. One already signed
    // in from before it was a plugin starts on, so its models stay.
    fn default_enabled(&self) -> bool {
        providers::starts_on(self.id())
    }

    fn settings(&self, cx: &App) -> Option<AnyElement> {
        let shared = providers::chatgpt::shares_codex();
        let muted = cx.theme().muted_foreground;
        Some(
            h_flex()
                .items_center()
                .justify_between()
                .gap_3()
                .pt_1()
                .child(
                    v_flex()
                        .flex_1()
                        .min_w_0()
                        .child(div().text_sm().child("Use the Codex app's sign-in"))
                        .child(div().text_xs().text_color(muted).child(
                            "Reads ~/.codex/auth.json on each request and never changes it. \
                             When it runs out, use Codex once to renew it.",
                        )),
                )
                .child(
                    div().flex_shrink_0().cursor_pointer().child(
                        crate::ui::settings::KeyedSwitch::new("chatgpt-codex-sign-in")
                            .checked(shared)
                            .on_click(move |_, _, cx| {
                                providers::save_switch(providers::chatgpt::CODEX_SWITCH, !shared);
                                cx.refresh_windows();
                            }),
                    ),
                )
                .into_any_element(),
        )
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
