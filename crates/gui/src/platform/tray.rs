//! The notification-area / system-tray icon.
//!
//! Windows draws its own themed popup when the icon is clicked, so the menu
//! matches the app; a double-click opens the window. Linux goes through KSNI,
//! which speaks StatusNotifierItem over D-Bus from a worker thread: no GTK
//! main loop to reconcile with gpui's, and the panel draws the menu itself,
//! so there the menu is native. macOS puts the icon in the menu bar, top
//! right, and either click opens the system's own menu, which also says how
//! things stand: the account, the model, what is running.
//!
//! On Windows and macOS a click shows the menu rather than the window: it is
//! the way into the app, a new session, the sessions used last, Settings,
//! and the way out of it, Quit, without finding the window first.

use crate::prelude::*;

/// Install the tray icon, if this platform has one.
pub(crate) fn init(cx: &mut App) {
    imp::init(cx);
}

/// Whether closing the window should leave the app running. Without a tray
/// there would be no way back, so the app quits instead. On macOS the window
/// goes to the Dock, and the menu bar icon or the Dock icon brings it back.
pub(crate) fn available() -> bool {
    cfg!(any(windows, target_os = "linux", target_os = "macos"))
}

fn main_window(cx: &App) -> Option<AnyWindowHandle> {
    cx.windows().into_iter().next()
}

/// Do `then` in the main window's chat, leaving the window where it is.
fn in_main(cx: &mut App, then: impl FnOnce(&mut Chat, &mut Window, &mut Context<Chat>)) {
    let Some(handle) = main_window(cx) else {
        return;
    };
    let _ = handle.update(cx, |_, window, cx| {
        let view = Root::read(window, cx).view().clone();
        if let Ok(chat) = view.downcast::<Chat>() {
            chat.update(cx, |this, cx| then(this, window, cx));
        }
    });
}

/// Bring the window forward, then do `then` in its chat: what a tray item
/// opens is opened where the user can see it.
fn open_main(cx: &mut App, then: impl FnOnce(&mut Chat, &mut Window, &mut Context<Chat>)) {
    in_main(cx, |this, window, cx| {
        super::window::show(window);
        then(this, window, cx);
    });
}

/// Put the window back in front, minimised, hidden or behind others; also
/// what a click on the Dock icon does while the window is tucked away.
pub(crate) fn show_main_window(cx: &mut App) {
    open_main(cx, |_, _, _| {});
}

/// Bring the window forward on the About page and look for a new version.
fn check_for_updates(cx: &mut App) {
    open_main(cx, |this, window, cx| {
        this.open_about(window, cx);
        this.check_for_updates(true, cx);
    });
}

/// Bring the window forward on a new session, ready to type in.
#[cfg(any(windows, target_os = "macos"))]
fn new_session(cx: &mut App) {
    open_main(cx, |this, window, cx| {
        this.page = Page::Chat;
        this.new_chat(window, cx);
    });
}

/// Bring the window forward on session `id`, as a click on a notification
/// about it does.
#[cfg(any(windows, target_os = "macos"))]
fn open_session(id: u64, cx: &mut App) {
    in_main(cx, move |this, window, cx| this.open_told(id, window, cx));
}

#[cfg(any(windows, target_os = "macos"))]
fn open_settings(cx: &mut App) {
    open_main(cx, |this, window, cx| {
        if !this.settings_open {
            this.open_settings(window, cx);
        }
    });
}

/// Bring the window forward on what is running, from the menu's line that
/// counts it.
#[cfg(any(windows, target_os = "macos"))]
fn open_running(cx: &mut App) {
    open_main(cx, |this, window, cx| this.open_activity(window, cx));
}

/// How many sessions the menu offers: those of the last day or so of work,
/// few enough that the menu stays a menu.
#[cfg(any(windows, target_os = "macos", test))]
const RECENT: usize = 6;

/// How many characters of a session's title fit the native menu before it
/// is cut; the menu grows to its longest row, and a title can be a paragraph.
#[cfg(any(target_os = "macos", test))]
const TITLE_CHARS: usize = 40;

/// The sessions the menu offers to open, by index, the one used last first.
/// A new one is left out until something is said in it, as in the sidebar.
#[cfg(any(windows, target_os = "macos", test))]
fn recent(conversations: &[Conversation], limit: usize) -> Vec<usize> {
    let mut used: Vec<usize> = (0..conversations.len())
        .filter(|&i| !untouched(&conversations[i]))
        .collect();
    used.sort_by_key(|&i| std::cmp::Reverse(conversations[i].updated));
    used.truncate(limit);
    used
}

/// What a session is waiting on, in the words the menu gives it, if
/// anything: what calls the user back first.
#[cfg(any(windows, target_os = "macos", test))]
fn waiting_on(state: crate::ui::sessions::SessionState) -> Option<&'static str> {
    if state.needs_you {
        Some("needs you")
    } else if state.working {
        Some("working")
    } else if state.unseen {
        Some("new")
    } else {
        None
    }
}

/// `title` cut to `max` characters, with an ellipsis when anything went.
#[cfg(any(target_os = "macos", test))]
fn cut(title: &str, max: usize) -> String {
    if title.chars().count() <= max {
        return title.to_string();
    }
    let kept: String = title.chars().take(max.saturating_sub(1)).collect();
    format!("{}…", kept.trim_end())
}

/// A session the menu offers to open.
#[cfg(any(windows, target_os = "macos"))]
#[derive(Clone, PartialEq)]
struct Recent {
    id: u64,
    title: String,
    waiting: Option<&'static str>,
}

#[cfg(any(windows, target_os = "macos"))]
fn recent_sessions(chat: &Chat) -> Vec<Recent> {
    recent(&chat.store.conversations, RECENT)
        .into_iter()
        .map(|i| Recent {
            id: chat.store.conversations[i].id,
            title: chat.store.conversations[i].shown_title(),
            waiting: waiting_on(chat.session_state(i)),
        })
        .collect()
}

/// How things stand, as the menu says it.
#[cfg(any(windows, target_os = "macos"))]
struct Standing {
    account: String,
    credit: String,
    model: String,
    running: usize,
}

#[cfg(any(windows, target_os = "macos"))]
fn dollars(cents: i64) -> String {
    format!("${}.{:02}", cents / 100, cents.rem_euclid(100))
}

#[cfg(any(windows, target_os = "macos"))]
fn standing(chat: &Chat) -> Standing {
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
    Standing {
        account,
        credit,
        model,
        running: chat.running_count(),
    }
}

#[cfg(any(windows, target_os = "macos", test))]
fn running_text(running: usize) -> String {
    match running {
        0 => "Nothing running".to_string(),
        1 => "1 task running".to_string(),
        n => format!("{n} tasks running"),
    }
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

    use super::{
        Recent, Standing, check_for_updates, main_window, new_session, open_running, open_session,
        open_settings, recent_sessions, running_text, show_main_window, standing,
    };

    const MENU_WIDTH: Pixels = px(248.);
    /// A row that does something when clicked, and the header's.
    const ROW: Pixels = px(28.);
    /// A line of how things stand, or the heading over the sessions.
    const LINE: Pixels = px(20.);
    /// The rule between groups: a pixel, with `my_1` either side.
    const RULE: Pixels = px(9.);
    /// The menu's `p_1` either side, and its border.
    const FRAME: Pixels = px(10.);

    struct Tray(#[allow(dead_code)] TrayIcon);
    impl Global for Tray {}

    /// The menu while it is open, so a click on the icon closes it again.
    struct OpenMenu(WindowHandle<TrayMenu>);
    impl Global for OpenMenu {}

    /// When the menu last closed for losing the focus. A press on the icon
    /// takes the focus from it, so the click meant to close the menu comes
    /// after the menu has already gone.
    struct ClosedAt(std::time::Instant);
    impl Global for ClosedAt {}

    /// How soon after the menu lost the focus a click on the icon is taken
    /// as the one that closed it, rather than one to open it again.
    const CLOSING_CLICK: Duration = Duration::from_millis(400);

    pub(super) fn init(cx: &mut App) {
        let Ok(icon) = Icon::from_resource(1, None) else {
            return;
        };
        let Ok(tray) = TrayIconBuilder::new()
            .with_tooltip(format!("{BRAND}: click for the menu, double-click to open"))
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
            // Windows tells a double-click between its two presses, and the
            // second one's release would open the menu again.
            let mut after_double = false;
            while let Ok(event) = rx.recv().await {
                match event {
                    TrayIconEvent::DoubleClick {
                        button: MouseButton::Left,
                        ..
                    } => {
                        after_double = true;
                        cx.update(|cx| {
                            close_menu(cx);
                            show_main_window(cx);
                        });
                    }
                    TrayIconEvent::Click {
                        button: MouseButton::Left | MouseButton::Right,
                        button_state: MouseButtonState::Up,
                        position,
                        ..
                    } => {
                        if std::mem::take(&mut after_double) {
                            continue;
                        }
                        cx.update(|cx| toggle_menu(position.x as f32, position.y as f32, cx));
                    }
                    _ => {}
                }
            }
        })
        .detach();
    }

    /// Open the menu at the pointer, or close it when it is open.
    fn toggle_menu(x: f32, y: f32, cx: &mut App) {
        let just_closed =
            (cx.try_global::<ClosedAt>()).is_some_and(|at| at.0.elapsed() < CLOSING_CLICK);
        if close_menu(cx) || just_closed {
            return;
        }
        open_menu(x, y, cx);
    }

    /// Close the menu; whether it was open.
    fn close_menu(cx: &mut App) -> bool {
        if !cx.has_global::<OpenMenu>() {
            return false;
        }
        let OpenMenu(handle) = cx.remove_global::<OpenMenu>();
        handle
            .update(cx, |_, window, _| window.remove_window())
            .is_ok()
    }

    /// How tall the menu is with `recent` sessions listed: every row it
    /// draws, as `TrayMenu::render` lays them out.
    fn menu_height(recent: usize) -> Pixels {
        // The header, Open, New session, Quick entry, Settings, Check for
        // updates and Quit, and a row per session.
        let rows = 7 + recent;
        // How things stand, and the heading over the sessions.
        let lines = 4 + usize::from(recent > 0);
        let rules = 3 + usize::from(recent > 0);
        FRAME + ROW * rows as f32 + LINE * lines as f32 + RULE * rules as f32
    }

    fn open_menu(x: f32, y: f32, cx: &mut App) {
        let Some(chat) = cx.try_global::<crate::ui::quick::MainChat>() else {
            return;
        };
        let chat = chat.0.read(cx);
        let (recent, standing) = (recent_sessions(chat), standing(chat));
        let size = size(MENU_WIDTH, menu_height(recent.len()));
        let scale = main_window(cx)
            .and_then(|h| h.update(cx, |_, window, _| window.scale_factor()).ok())
            .unwrap_or(1.);
        // Up and to the left of the pointer, as the taskbar is usually at
        // the bottom; never above the top of the screen.
        let origin = point(
            (px(x / scale) - size.width).max(px(0.)),
            (px(y / scale) - size.height).max(px(0.)),
        );
        let opened = cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(Bounds { origin, size })),
                titlebar: None,
                kind: WindowKind::PopUp,
                is_movable: false,
                is_resizable: false,
                is_minimizable: false,
                focus: true,
                show: true,
                ..Default::default()
            },
            move |window, cx| cx.new(|cx| TrayMenu::new(recent, standing, window, cx)),
        );
        if let Ok(handle) = opened {
            cx.set_global(OpenMenu(handle));
        }
    }

    struct TrayMenu {
        recent: Vec<Recent>,
        standing: Standing,
        _activation: Subscription,
    }

    impl TrayMenu {
        fn new(
            recent: Vec<Recent>,
            standing: Standing,
            window: &mut Window,
            cx: &mut Context<Self>,
        ) -> Self {
            crate::platform::window::make_topmost(window);
            let opened = std::time::Instant::now();
            let activation = cx.observe_window_activation(window, move |_, window, cx| {
                if !window.is_window_active() && opened.elapsed() > Duration::from_millis(200) {
                    window.remove_window();
                    cx.set_global(ClosedAt(std::time::Instant::now()));
                }
            });
            Self {
                recent,
                standing,
                _activation: activation,
            }
        }

        fn item(
            &self,
            id: impl Into<ElementId>,
            label: impl IntoElement,
            cx: &mut Context<Self>,
            on_click: impl Fn(&mut Window, &mut App) + 'static,
        ) -> Stateful<Div> {
            h_flex()
                .id(id)
                .h(ROW)
                .px_2()
                .gap_2()
                .items_center()
                .rounded(cx.theme().radius)
                .cursor_pointer()
                .active(|s| s.bg(cx.theme().secondary))
                .child(label)
                .on_click(move |_, window, cx| {
                    on_click(window, cx);
                    window.remove_window();
                })
        }

        /// A line that only informs, in the muted colour.
        fn line(&self, text: impl Into<SharedString>, cx: &mut Context<Self>) -> Div {
            div()
                .h(LINE)
                .px_2()
                .flex()
                .items_center()
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .child(div().min_w_0().truncate().child(text.into()))
        }

        /// A recent session's row: its title, and what it waits on.
        fn session_row(&self, recent: &Recent, cx: &mut Context<Self>) -> Stateful<Div> {
            let id = recent.id;
            let label = h_flex()
                .flex_1()
                .min_w_0()
                .gap_2()
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .child(recent.title.clone()),
                )
                .children(recent.waiting.map(|waiting| {
                    div()
                        .flex_shrink_0()
                        .text_xs()
                        .text_color(if waiting == "needs you" {
                            cx.theme().warning
                        } else {
                            cx.theme().muted_foreground
                        })
                        .child(waiting)
                }));
            self.item(("tray-session", id), label, cx, move |_, cx| {
                open_session(id, cx)
            })
        }
    }

    impl Render for TrayMenu {
        fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            let border = cx.theme().border;
            let rule = move || div().h(px(1.)).my_1().bg(border);
            let running = self.standing.running;
            let recent = self.recent.clone();
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
                        .h(ROW)
                        .px_2()
                        .gap_2()
                        .items_center()
                        .child(img("images/letronna.png").size(px(16.)).rounded(px(3.)))
                        .child(
                            div()
                                .font_family(HEADING_FONT)
                                .font_weight(FontWeight::SEMIBOLD)
                                .child(BRAND),
                        )
                        .child(
                            div()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child(env!("CARGO_PKG_VERSION")),
                        ),
                )
                .child(self.line(self.standing.account.clone(), cx))
                .child(self.line(self.standing.credit.clone(), cx))
                .child(self.line(self.standing.model.clone(), cx))
                // With something running, it opens what is.
                .child(self.line(running_text(running), cx).id("running").when(
                    running > 0,
                    |this| {
                        this.cursor_pointer().text_color(cx.theme().link).on_click(
                            |_, window, cx| {
                                open_running(cx);
                                window.remove_window();
                            },
                        )
                    },
                ))
                .child(rule())
                .child(self.item("open", format!("Open {BRAND}"), cx, |_, cx| {
                    show_main_window(cx)
                }))
                .child(self.item("new", "New session", cx, |_, cx| new_session(cx)))
                .child(self.item("quick", "Quick entry", cx, |_, cx| {
                    crate::ui::quick::toggle(cx)
                }))
                .when(!recent.is_empty(), |this| {
                    this.child(rule())
                        .child(self.line("Recent sessions", cx))
                        .children(recent.iter().map(|r| self.session_row(r, cx)))
                })
                .child(rule())
                .child(self.item("settings", "Settings", cx, |_, cx| open_settings(cx)))
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
    use super::{
        Recent, TITLE_CHARS, check_for_updates, cut, new_session, open_running, open_session,
        open_settings, recent_sessions, running_text, show_main_window, standing,
    };
    use crate::prelude::*;
    use std::time::Duration;
    use tray_icon::menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem, Submenu};
    use tray_icon::{Icon, TrayIcon, TrayIconBuilder};

    /// The icon, and the rows that say how things stand, rewritten as they
    /// change.
    struct Tray {
        _icon: TrayIcon,
        account: MenuItem,
        credit: MenuItem,
        model: MenuItem,
        running: MenuItem,
        recent: Submenu,
        /// What `recent` lists now, so it is rebuilt only when that changes.
        listed: Vec<Recent>,
    }
    impl Global for Tray {}

    /// Ids are matched back from the menu event, so keep them stable.
    const SHOW: &str = "show";
    const NEW_SESSION: &str = "new-session";
    const QUICK_ENTRY: &str = "quick-entry";
    const SETTINGS: &str = "settings";
    const RUNNING: &str = "running";
    const UPDATES: &str = "updates";
    const QUIT: &str = "quit";
    /// Followed by the session's id.
    const SESSION: &str = "session:";

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
        let (account, credit, model) = (info(""), info(""), info(""));
        // Picked, it shows what runs; with nothing running it is greyed.
        let running = MenuItem::with_id(RUNNING, "", false, None);
        let show = MenuItem::with_id(SHOW, format!("Open {BRAND}"), true, None);
        let new_session = MenuItem::with_id(NEW_SESSION, "New session", true, None);
        let quick_entry = MenuItem::with_id(QUICK_ENTRY, "Quick entry", true, None);
        let recent = Submenu::new("Recent sessions", false);
        let settings = MenuItem::with_id(SETTINGS, "Settings…", true, None);
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
                &new_session,
                &quick_entry,
                &recent,
                &PredefinedMenuItem::separator(),
                &settings,
                &updates,
                &PredefinedMenuItem::separator(),
                &quit,
            ])
            .is_err()
        {
            return;
        }
        let mut builder = TrayIconBuilder::new()
            .with_tooltip(BRAND)
            .with_menu(Box::new(menu))
            // Either click opens the menu, as a menu bar icon's does; Open
            // in it, the Dock icon or a notification bring the window.
            .with_menu_on_left_click(true);
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
            recent,
            listed: Vec::new(),
        });

        // Handed over as they happen, as on Windows. The app comes to the
        // front there and then, while the pick is still the event being
        // handled: macOS lets an app forward in answer to the user, and may
        // refuse it a moment later.
        let (picks, picked_ids) = async_channel::unbounded();
        MenuEvent::set_event_handler(Some(move |event: MenuEvent| {
            // The quick entry bar is a panel that leaves the app where it
            // was, and Quit has nothing to show.
            if event.id.0 != QUICK_ENTRY && event.id.0 != QUIT {
                crate::platform::window::activate_app();
            }
            let _ = picks.send_blocking(event.id.0);
        }));
        cx.spawn(async move |cx| {
            while let Ok(id) = picked_ids.recv().await {
                cx.update(|cx| picked(&id, cx));
            }
        })
        .detach();
        cx.spawn(async move |cx| {
            loop {
                cx.update(refresh);
                // Fresh enough for a menu opened by hand.
                cx.background_executor().timer(Duration::from_secs(2)).await;
            }
        })
        .detach();
    }

    /// Do what the menu item with `id` says.
    fn picked(id: &str, cx: &mut App) {
        match id {
            SHOW => show_main_window(cx),
            NEW_SESSION => new_session(cx),
            QUICK_ENTRY => crate::ui::quick::toggle(cx),
            SETTINGS => open_settings(cx),
            RUNNING => open_running(cx),
            UPDATES => check_for_updates(cx),
            QUIT => cx.quit(),
            _ => {
                if let Some(session) = id.strip_prefix(SESSION).and_then(|s| s.parse().ok()) {
                    open_session(session, cx);
                }
            }
        }
    }

    /// A recent session's row: its title, cut to fit, and what it waits on.
    fn session_item(recent: &Recent) -> MenuItem {
        let title = cut(&recent.title, TITLE_CHARS);
        let text = match recent.waiting {
            Some(waiting) => format!("{title} · {waiting}"),
            None => title,
        };
        MenuItem::with_id(format!("{SESSION}{}", recent.id), text, true, None)
    }

    /// Rewrite the rows from the window's own state.
    fn refresh(cx: &mut App) {
        let Some(chat) = cx.try_global::<crate::ui::quick::MainChat>() else {
            return;
        };
        let chat = chat.0.read(cx);
        let standing = standing(chat);
        let recent = recent_sessions(chat);
        let tray = cx.global_mut::<Tray>();
        tray.account.set_text(standing.account);
        tray.credit.set_text(standing.credit);
        tray.model.set_text(standing.model);
        tray.running.set_text(running_text(standing.running));
        tray.running.set_enabled(standing.running > 0);
        if tray.listed == recent {
            return;
        }
        while tray.recent.remove_at(0).is_some() {}
        for session in &recent {
            let _ = tray.recent.append(&session_item(session));
        }
        tray.recent.set_enabled(!recent.is_empty());
        tray.listed = recent;
    }
}

#[cfg(test)]
mod tests {
    use super::{RECENT, TITLE_CHARS, cut, recent, running_text, waiting_on};
    use crate::ui::sessions::SessionState;
    use letronna_core::store::Conversation;

    fn used(updated: u64) -> Conversation {
        Conversation {
            title: format!("Session {updated}"),
            updated,
            ..Default::default()
        }
    }

    #[test]
    fn the_menu_offers_the_sessions_used_last_and_no_new_empty_one() {
        let fresh = Conversation {
            title: "New session".to_string(),
            updated: 99,
            ..Default::default()
        };
        let conversations = vec![used(10), used(30), fresh, used(20)];
        assert_eq!(recent(&conversations, RECENT), [1, 3, 0]);
        assert_eq!(recent(&conversations, 2), [1, 3]);
    }

    #[test]
    fn a_long_title_is_cut_to_fit_the_menu() {
        assert_eq!(cut("Fix the build", TITLE_CHARS), "Fix the build");
        let long = "word ".repeat(20);
        let shown = cut(&long, TITLE_CHARS);
        assert!(shown.chars().count() <= TITLE_CHARS);
        assert!(shown.ends_with("word…"));
        // Cut by characters, so a title in any script stays whole letters.
        assert_eq!(cut("日本語のタイトル", 4), "日本語…");
    }

    #[test]
    fn what_calls_you_back_is_said_first() {
        let state = |needs_you, working, unseen| SessionState {
            needs_you,
            working,
            unseen,
            pinned: false,
        };
        assert_eq!(waiting_on(state(true, true, true)), Some("needs you"));
        assert_eq!(waiting_on(state(false, true, true)), Some("working"));
        assert_eq!(waiting_on(state(false, false, true)), Some("new"));
        assert_eq!(waiting_on(state(false, false, false)), None);
    }

    #[test]
    fn the_running_line_counts_in_words() {
        assert_eq!(running_text(0), "Nothing running");
        assert_eq!(running_text(1), "1 task running");
        assert_eq!(running_text(3), "3 tasks running");
    }
}
