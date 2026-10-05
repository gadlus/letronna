//! The notification-area / system-tray icon.
//!
//! Windows draws its own themed popup on right click so the menu matches the
//! app. Linux goes through KSNI, which speaks StatusNotifierItem over D-Bus
//! from a worker thread: no GTK main loop to reconcile with gpui's, and the
//! panel draws the menu itself, so there the menu is native.

use crate::prelude::*;

/// Install the tray icon, if this platform has one.
pub(crate) fn init(cx: &mut App) {
    #[cfg(any(windows, target_os = "linux"))]
    imp::init(cx);
    #[cfg(not(any(windows, target_os = "linux")))]
    let _ = cx;
}

/// Whether closing the window should leave the app running. Without a tray
/// there would be no way back, so the app quits instead.
pub(crate) fn available() -> bool {
    cfg!(any(windows, target_os = "linux"))
}

#[cfg(any(windows, target_os = "linux"))]
fn main_window(cx: &App) -> Option<AnyWindowHandle> {
    cx.windows().into_iter().next()
}

#[cfg(any(windows, target_os = "linux"))]
fn show_main_window(cx: &mut App) {
    if let Some(handle) = main_window(cx) {
        let _ = handle.update(cx, |_, window, _| super::window::show(window));
    }
}

/// Bring the window forward on the About page and look for a new version.
#[cfg(any(windows, target_os = "linux"))]
fn check_for_updates(cx: &mut App) {
    show_main_window(cx);
    let Some(handle) = main_window(cx) else {
        return;
    };
    let _ = handle.update(cx, |_, window, cx| {
        let view = Root::read(window, cx).view().clone();
        if let Ok(chat) = view.downcast::<Chat>() {
            chat.update(cx, |this, cx| {
                this.open_about(window, cx);
                this.check_for_updates(true, cx);
            });
        }
    });
}

#[cfg(windows)]
use windows_impl as imp;

#[cfg(target_os = "linux")]
use linux_impl as imp;

#[cfg(target_os = "linux")]
mod linux_impl {
    use super::{check_for_updates, show_main_window};
    use crate::prelude::*;
    use std::time::Duration;
    use tray_icon::menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem};
    use tray_icon::{Icon, TrayIcon, TrayIconBuilder};

    struct Tray(#[allow(dead_code)] TrayIcon);
    impl Global for Tray {}

    /// Ids are matched back from the menu event, so keep them stable.
    const SHOW: &str = "show";
    const QUICK_ENTRY: &str = "quick-entry";
    const UPDATES: &str = "updates";
    const QUIT: &str = "quit";

    /// The tray wants raw RGBA, and the app icon is already embedded as a PNG.
    fn icon() -> Option<Icon> {
        let bytes = crate::assets::Assets
            .load("images/letronna.png")
            .ok()
            .flatten()?;
        let image = image::load_from_memory(&bytes).ok()?.into_rgba8();
        let (width, height) = image.dimensions();
        Icon::from_rgba(image.into_raw(), width, height).ok()
    }

    pub(super) fn init(cx: &mut App) {
        let menu = Menu::new();
        let show = MenuItem::with_id(SHOW, format!("Open {BRAND}"), true, None);
        // Wayland grants no application a global key at all, so this is the
        // only way to the quick entry bar there; harmless to offer under X11
        // too, where the shortcut usually already works.
        let quick_entry = MenuItem::with_id(QUICK_ENTRY, "Quick entry", true, None);
        let updates = MenuItem::with_id(UPDATES, "Check for updates", true, None);
        let quit = MenuItem::with_id(QUIT, format!("Quit {BRAND}"), true, None);
        if menu
            .append_items(&[
                &show,
                &quick_entry,
                &PredefinedMenuItem::separator(),
                &updates,
                &quit,
            ])
            .is_err()
        {
            return;
        }
        let mut builder = TrayIconBuilder::new()
            .with_tooltip(BRAND)
            .with_menu(Box::new(menu));
        if let Some(icon) = icon() {
            builder = builder.with_icon(icon);
        }
        // A desktop with no StatusNotifierItem host has no tray; carry on
        // without one rather than refusing to start.
        let Ok(tray) = builder.build() else {
            return;
        };
        cx.set_global(Tray(tray));

        cx.spawn(async move |cx| {
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(200))
                    .await;
                while let Ok(event) = MenuEvent::receiver().try_recv() {
                    cx.update(|cx| match event.id.0.as_str() {
                        SHOW => show_main_window(cx),
                        QUICK_ENTRY => crate::ui::quick::toggle(cx),
                        UPDATES => check_for_updates(cx),
                        QUIT => cx.quit(),
                        _ => {}
                    });
                }
            }
        })
        .detach();
    }
}

#[cfg(windows)]
mod windows_impl {
    use crate::prelude::*;
    use std::time::Duration;
    use tray_icon::{
        Icon, MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent,
    };

    use super::{check_for_updates, main_window, show_main_window};
    const MENU_SIZE: Size<Pixels> = size(px(208.), px(112.));

    struct Tray(#[allow(dead_code)] TrayIcon);
    impl Global for Tray {}

    pub(super) fn init(cx: &mut App) {
        let Ok(icon) = Icon::from_resource(1, None) else {
            return;
        };
        let Ok(tray) = TrayIconBuilder::new()
            .with_tooltip(BRAND)
            .with_icon(icon)
            .with_menu_on_left_click(false)
            .build()
        else {
            return;
        };
        cx.set_global(Tray(tray));

        // Handed over as they happen, rather than looked for ten times a
        // second for as long as the app runs.
        let (tx, rx) = async_channel::unbounded();
        TrayIconEvent::set_event_handler(Some(move |event: TrayIconEvent| {
            let _ = tx.send_blocking(event);
        }));
        cx.spawn(async move |cx| {
            while let Ok(event) = rx.recv().await {
                let TrayIconEvent::Click {
                    button,
                    button_state: MouseButtonState::Up,
                    position,
                    ..
                } = event
                else {
                    continue;
                };
                cx.update(|cx| match button {
                    MouseButton::Left => show_main_window(cx),
                    MouseButton::Right => open_menu(position.x as f32, position.y as f32, cx),
                    MouseButton::Middle => {}
                });
            }
        })
        .detach();
    }

    fn open_menu(x: f32, y: f32, cx: &mut App) {
        let scale = main_window(cx)
            .and_then(|h| h.update(cx, |_, window, _| window.scale_factor()).ok())
            .unwrap_or(1.);
        let origin = point(
            px(x / scale) - MENU_SIZE.width,
            px(y / scale) - MENU_SIZE.height,
        );
        let _ = cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(Bounds {
                    origin,
                    size: MENU_SIZE,
                })),
                titlebar: None,
                kind: WindowKind::PopUp,
                is_movable: false,
                is_resizable: false,
                is_minimizable: false,
                focus: true,
                show: true,
                ..Default::default()
            },
            |window, cx| cx.new(|cx| TrayMenu::new(window, cx)),
        );
    }

    struct TrayMenu {
        _activation: Subscription,
    }

    impl TrayMenu {
        fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
            crate::platform::window::make_topmost(window);
            let opened = std::time::Instant::now();
            let activation = cx.observe_window_activation(window, move |_, window, _| {
                if !window.is_window_active() && opened.elapsed() > Duration::from_millis(200) {
                    window.remove_window();
                }
            });
            Self {
                _activation: activation,
            }
        }

        fn item(
            &self,
            id: &'static str,
            label: impl Into<SharedString>,
            cx: &mut Context<Self>,
            on_click: impl Fn(&mut Window, &mut App) + 'static,
        ) -> impl IntoElement {
            div()
                .id(id)
                .h(px(28.))
                .px_2()
                .flex()
                .items_center()
                .rounded(cx.theme().radius)
                .cursor_pointer()
                .active(|s| s.bg(cx.theme().secondary))
                .child(label.into())
                .on_click(move |_, window, cx| {
                    on_click(window, cx);
                    window.remove_window();
                })
        }
    }

    impl Render for TrayMenu {
        fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            let border = cx.theme().border;
            let rule = move || div().h(px(1.)).my_1().bg(border);
            v_flex()
                .size_full()
                .p_1()
                .bg(cx.theme().background)
                .border_1()
                .border_color(cx.theme().border)
                .rounded(cx.theme().radius_lg)
                .text_sm()
                .font_family(BODY_FONT)
                .text_color(cx.theme().foreground)
                .child(
                    h_flex()
                        .h(px(28.))
                        .px_2()
                        .gap_2()
                        .items_center()
                        .child(img("images/letronna.png").size(px(16.)).rounded(px(3.)))
                        .child(
                            div()
                                .font_family(HEADING_FONT)
                                .font_weight(FontWeight::SEMIBOLD)
                                .child(BRAND),
                        ),
                )
                .child(rule())
                .child(self.item("updates", "Check for updates", cx, |_, cx| {
                    check_for_updates(cx)
                }))
                .child(rule())
                .child(self.item("quit", format!("Quit {BRAND}"), cx, |_, cx| cx.quit()))
                .with_animation(
                    "tray-menu",
                    Animation::new(std::time::Duration::from_millis(160))
                        .with_easing(gpui::ease_out_quint()),
                    |el, delta| el.opacity(delta).mt(px(8. * (1. - delta))),
                )
        }
    }
}
