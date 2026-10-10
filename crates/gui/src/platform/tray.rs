//! The notification-area / system-tray icon.
//!
//! Windows draws its own themed popup on right click so the menu matches the
//! app. Linux goes through KSNI, which speaks StatusNotifierItem over D-Bus
//! from a worker thread: no GTK main loop to reconcile with gpui's, and the
//! panel draws the menu itself, so there the menu is native. macOS puts the
//! icon in the menu bar, top right, with the system's own menu on right click
//! that also says how things stand: the account, the model, what is running.

use crate::prelude::*;

/// Install the tray icon, if this platform has one.
pub(crate) fn init(cx: &mut App) {
    imp::init(cx);
}

/// Whether closing the window should leave the app running. Without a tray
/// there would be no way back, so the app quits instead. On macOS closing the
/// window quits as before: the menu bar icon is for while it is open.
pub(crate) fn available() -> bool {
    cfg!(any(windows, target_os = "linux"))
}

fn main_window(cx: &App) -> Option<AnyWindowHandle> {
    cx.windows().into_iter().next()
}

fn show_main_window(cx: &mut App) {
    if let Some(handle) = main_window(cx) {
        let _ = handle.update(cx, |_, window, _| super::window::show(window));
    }
}

/// Bring the window forward on the About page and look for a new version.
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

#[cfg(target_os = "macos")]
use macos_impl as imp;

#[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
mod imp {
    pub(super) fn init(_: &mut crate::prelude::App) {}
}

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
                    crate::ui::motion::enter(crate::ui::motion::QUICK),
                    |el, delta| {
                        el.opacity(delta)
                            .mt(px(crate::ui::motion::RISE * (1. - delta)))
                    },
                )
        }
    }
}

#[cfg(target_os = "macos")]
mod macos_impl {
    use super::{check_for_updates, show_main_window};
    use crate::prelude::*;
    use std::time::Duration;
    use tray_icon::menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem};
    use tray_icon::{
        Icon, MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent,
    };

    /// The icon, and the rows that say how things stand, rewritten as they
    /// change.
    struct Tray {
        _icon: TrayIcon,
        account: MenuItem,
        credit: MenuItem,
        model: MenuItem,
        running: MenuItem,
    }
    impl Global for Tray {}

    /// Ids are matched back from the menu event, so keep them stable.
    const SHOW: &str = "show";
    const QUICK_ENTRY: &str = "quick-entry";
    const UPDATES: &str = "updates";
    const QUIT: &str = "quit";

    /// The portrait's ink alone, in a rounded frame: a template image, which
    /// macOS tints to match the menu bar, light or dark, like its own icons.
    fn icon() -> Option<Icon> {
        let bytes = include_bytes!("../../assets/letronna-menubar.png");
        let image = image::load_from_memory(bytes).ok()?.into_rgba8();
        let (width, height) = image.dimensions();
        Icon::from_rgba(image.into_raw(), width, height).ok()
    }

    /// A row that only informs: greyed out, does nothing when picked.
    fn info(text: &str) -> MenuItem {
        MenuItem::new(text, false, None)
    }

    pub(super) fn init(cx: &mut App) {
        let menu = Menu::new();
        let version = info(&format!("{BRAND} {}", env!("CARGO_PKG_VERSION")));
        let (account, credit, model, running) = (info(""), info(""), info(""), info(""));
        let show = MenuItem::with_id(SHOW, format!("Open {BRAND}"), true, None);
        let quick_entry = MenuItem::with_id(QUICK_ENTRY, "Quick entry", true, None);
        let updates = MenuItem::with_id(UPDATES, "Check for updates", true, None);
        let quit = MenuItem::with_id(QUIT, format!("Quit {BRAND}"), true, None);
        if menu
            .append_items(&[
                &version,
                &account,
                &credit,
                &model,
                &running,
                &PredefinedMenuItem::separator(),
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
            .with_menu(Box::new(menu))
            // Left click opens the window; the menu is for the right.
            .with_menu_on_left_click(false);
        if let Some(icon) = icon() {
            builder = builder.with_icon(icon).with_icon_as_template(true);
        }
        let Ok(icon) = builder.build() else {
            return;
        };
        cx.set_global(Tray {
            _icon: icon,
            account,
            credit,
            model,
            running,
        });

        cx.spawn(async move |cx| {
            let mut ticks = 0u32;
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(200))
                    .await;
                while let Ok(event) = TrayIconEvent::receiver().try_recv() {
                    if let TrayIconEvent::Click {
                        button: MouseButton::Left,
                        button_state: MouseButtonState::Up,
                        ..
                    } = event
                    {
                        cx.update(show_main_window);
                    }
                }
                while let Ok(event) = MenuEvent::receiver().try_recv() {
                    cx.update(|cx| match event.id.0.as_str() {
                        SHOW => show_main_window(cx),
                        QUICK_ENTRY => crate::ui::quick::toggle(cx),
                        UPDATES => check_for_updates(cx),
                        QUIT => cx.quit(),
                        _ => {}
                    });
                }
                // Every two seconds is fresh enough for a menu opened by hand.
                if ticks.is_multiple_of(10) {
                    cx.update(refresh);
                }
                ticks = ticks.wrapping_add(1);
            }
        })
        .detach();
    }

    fn dollars(cents: i64) -> String {
        format!("${}.{:02}", cents / 100, cents.rem_euclid(100))
    }

    /// Rewrite the rows from the window's own state.
    fn refresh(cx: &mut App) {
        let Some(chat) = cx.try_global::<crate::ui::quick::MainChat>() else {
            return;
        };
        let chat = chat.0.read(cx);
        let (account, credit) = match &chat.account {
            Some(a) => (
                format!("{} · {}", a.email, a.plan_name()),
                match &a.team {
                    Some(team) => format!("Team {}: {} left", team.name, dollars(team.credits)),
                    None => format!("Credit: {} left", dollars(a.credits)),
                },
            ),
            None => (
                "Not signed in".to_string(),
                "Credit: sign in to use hosted models".to_string(),
            ),
        };
        let model = match chat.store.settings.model.as_str() {
            "" => "Model: none picked yet".to_string(),
            m => format!("Model: {m}"),
        };
        let running = match chat.running_count() {
            0 => "Nothing running".to_string(),
            1 => "1 task running".to_string(),
            n => format!("{n} tasks running"),
        };
        let tray = cx.global::<Tray>();
        tray.account.set_text(account);
        tray.credit.set_text(credit);
        tray.model.set_text(model);
        tray.running.set_text(running);
    }
}
