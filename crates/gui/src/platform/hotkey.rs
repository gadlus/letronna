//! The global "quick entry" shortcut.
//!
//! `global_hotkey` delivers a press through a static handler, not a channel
//! tied to this app's loop, and that handler can run on whatever thread the
//! platform's key-grab lives on (its own thread on Windows and X11, the main
//! thread on macOS). It is forwarded down an `async_channel` and picked up by
//! a task that awaits it, so nothing here polls: the task only wakes when a
//! send actually happens.
//!
//! X11 is the only Linux backend the crate has; under Wayland
//! `GlobalHotKeyManager::new` itself fails, and [`init`] leaves the shortcut
//! unregistered rather than erroring, since the tray's "Quick entry" item is
//! still a way in.

use crate::prelude::*;
use global_hotkey::hotkey::HotKey;
use global_hotkey::{GlobalHotKeyEvent, GlobalHotKeyManager, HotKeyState};

pub(crate) const DEFAULT: &str = "ctrl+shift+space";

struct Registered {
    manager: GlobalHotKeyManager,
    current: Option<HotKey>,
}
impl Global for Registered {}

/// Register `spec` and start listening for it. Called once at startup; a
/// later change from Settings goes through [`rebind`] instead.
pub(crate) fn init(spec: &str, cx: &mut App) {
    let Ok(manager) = GlobalHotKeyManager::new() else {
        return;
    };
    cx.set_global(Registered {
        manager,
        current: None,
    });
    rebind(spec, cx);

    let (tx, rx) = async_channel::unbounded();
    GlobalHotKeyEvent::set_event_handler(Some(move |event: GlobalHotKeyEvent| {
        let _ = tx.send_blocking(event);
    }));
    cx.spawn(async move |cx| {
        while let Ok(event) = rx.recv().await {
            if event.state() != HotKeyState::Pressed {
                continue;
            }
            cx.update(crate::ui::quick::toggle);
        }
    })
    .detach();
}

/// Swap the registered shortcut for `spec`. Returns whether it took: a spec
/// that does not parse, a platform with no manager (Wayland), or a chord
/// already grabbed by something else all report failure the same way, and
/// each leaves the shortcut that was registered before as it was.
pub(crate) fn rebind(spec: &str, cx: &mut App) -> bool {
    let Ok(hotkey) = spec.trim().parse::<HotKey>() else {
        return false;
    };
    if !cx.has_global::<Registered>() {
        return false;
    }
    let state = cx.global_mut::<Registered>();
    if state.current == Some(hotkey) {
        return true;
    }
    // The new chord first: a spec another program already holds must leave
    // the working shortcut alone, which Settings tells the user it did.
    if state.manager.register(hotkey).is_err() {
        return false;
    }
    if let Some(old) = state.current.replace(hotkey) {
        let _ = state.manager.unregister(old);
    }
    true
}

#[cfg(test)]
mod tests {
    // Not `use super::*`: the prelude re-exports gpui, whose `test` macro
    // would shadow the built-in attribute.
    use super::DEFAULT;
    use global_hotkey::hotkey::HotKey;

    #[test]
    fn the_default_shortcut_parses() {
        assert!(DEFAULT.parse::<HotKey>().is_ok());
    }

    #[test]
    fn a_spelling_slip_does_not_parse() {
        assert!("ctrl+shift+".parse::<HotKey>().is_err());
        assert!("hyper+spaec".parse::<HotKey>().is_err());
    }
}
