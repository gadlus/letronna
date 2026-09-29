//! The quick entry bar: a second, small window over whatever the user is
//! doing, opened by the global shortcut or the tray's "Quick entry" item.
//! It is a plain gpui window in this same process, so it reaches the same
//! [`Chat`] entity the main window renders -- there is nothing to boot and
//! nothing else to keep in sync.

use crate::prelude::*;
use crate::ui::pictures;
use gpui_component::input::Paste;

actions!(quick, [Hide, ToggleTarget]);

const CONTEXT: &str = "Quick";
const SIZE: Size<Pixels> = size(px(560.), px(56.));

/// The main window's [`Chat`], reachable from the quick entry window so a
/// sent prompt lands in the same store the main window reads and saves.
pub(crate) struct MainChat(pub(crate) Entity<Chat>);
impl Global for MainChat {}

/// The quick entry window, while one is open. Its absence is how [`toggle`]
/// knows whether to open a new one or dismiss the one already showing.
struct Open(WindowHandle<Root>);
impl Global for Open {}

pub(crate) fn init(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("escape", Hide, Some(CONTEXT)),
        KeyBinding::new("secondary-l", ToggleTarget, Some(CONTEXT)),
    ]);
}

/// Open the bar, or dismiss it if the shortcut was pressed again while it is
/// already up. Called from the global hotkey and the tray's fallback item.
pub(crate) fn toggle(cx: &mut App) {
    if cx.has_global::<Open>() {
        let Open(handle) = cx.remove_global::<Open>();
        let _ = handle.update(cx, |_, window, _| window.remove_window());
        return;
    }
    let Some(chat) = cx.try_global::<MainChat>().map(|m| m.0.clone()) else {
        return;
    };
    let Ok(handle) = cx.open_window(
        WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(Bounds::centered(None, SIZE, cx))),
            titlebar: None,
            kind: WindowKind::PopUp,
            is_movable: false,
            is_resizable: false,
            is_minimizable: false,
            focus: true,
            show: true,
            ..Default::default()
        },
        move |window, cx| {
            let bar = cx.new(|cx| QuickEntry::new(chat.clone(), window, cx));
            // The input reads and writes its focus through the window's
            // `Root` as it paints, and panics on a window that has none.
            cx.new(|cx| Root::new(bar, window, cx))
        },
    ) else {
        return;
    };
    cx.set_global(Open(handle));
}

struct QuickEntry {
    main: Entity<Chat>,
    input: Entity<InputState>,
    new_session: bool,
    /// Pictures pasted into the bar, waiting to go with the next send.
    images: Vec<PathBuf>,
    _subscriptions: Vec<Subscription>,
}

impl QuickEntry {
    fn new(main: Entity<Chat>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let input = cx.new(|cx| InputState::new(window, cx).placeholder("Ask Letronna\u{2026}"));
        input.update(cx, |input, cx| input.focus(window, cx));
        let subscriptions = vec![
            cx.subscribe_in(&input, window, |this, _, event, window, cx| {
                if matches!(event, InputEvent::PressEnter { .. }) {
                    this.send(window, cx);
                }
            }),
        ];
        Self {
            main,
            input,
            new_session: false,
            images: Vec::new(),
            _subscriptions: subscriptions,
        }
    }

    fn send(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let prompt = self.input.read(cx).value().trim().to_owned();
        if prompt.is_empty() && self.images.is_empty() {
            return;
        }
        let images = std::mem::take(&mut self.images);
        let new_session = self.new_session;
        self.main.update(cx, |chat, cx| {
            let _ = chat.quick_dispatch(prompt, images, new_session, cx);
        });
        self.hide(window, cx);
    }

    fn hide(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        cx.remove_global::<Open>();
        window.remove_window();
    }

    fn toggle_target(&mut self, cx: &mut Context<Self>) {
        self.new_session = !self.new_session;
        cx.notify();
    }

    /// Attach the picture on the clipboard, as the composer's own paste does:
    /// scaled and saved through the same pipeline, staged here rather than on
    /// a session, since the bar has not yet decided which one this goes to.
    fn paste_image(&mut self, _: &Paste, window: &mut Window, cx: &mut Context<Self>) {
        let Some(item) = cx.read_from_clipboard() else {
            return;
        };
        // Copied text can bring a picture of itself along, as Office's does.
        if item
            .entries()
            .iter()
            .any(|entry| matches!(entry, ClipboardEntry::String(_)))
        {
            return;
        }
        let Some(bytes) = item.entries().iter().find_map(|entry| match entry {
            ClipboardEntry::Image(image) => Some(image.bytes.clone()),
            _ => None,
        }) else {
            return;
        };
        cx.stop_propagation();
        let saving = cx.background_executor().spawn(async move {
            pictures::fit(&bytes).and_then(|bytes| letronna_core::media::attach("pasted", &bytes))
        });
        cx.spawn_in(window, async move |this, cx| {
            if let Ok(path) = saving.await {
                let _ = this.update(cx, |this, cx| {
                    this.images.push(path);
                    cx.notify();
                });
            }
        })
        .detach();
    }
}

impl Render for QuickEntry {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let target_label = if self.new_session {
            "New session"
        } else {
            "Continue last"
        };
        h_flex()
            .key_context(CONTEXT)
            .on_action(cx.listener(|this, _: &Hide, window, cx| this.hide(window, cx)))
            .on_action(cx.listener(|this, _: &ToggleTarget, _, cx| this.toggle_target(cx)))
            .capture_action(cx.listener(Self::paste_image))
            .size_full()
            .items_center()
            .gap_2()
            .px_3()
            .rounded(cx.theme().radius_lg)
            .border_1()
            .border_color(cx.theme().border)
            .bg(cx.theme().background)
            .child(
                Icon::new(AppIcon::Sparkles)
                    .size_4()
                    .text_color(cx.theme().muted_foreground),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .child(Input::new(&self.input).appearance(false).w_full()),
            )
            .when(!self.images.is_empty(), |row| {
                row.child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(format!("{} image", self.images.len())),
                )
            })
            .child(
                Button::new("quick-target")
                    .plain(cx)
                    .xsmall()
                    .child(target_label)
                    .tooltip(if cfg!(target_os = "macos") {
                        "New session or the last one \u{2014} \u{2318}L"
                    } else {
                        "New session or the last one \u{2014} Ctrl+L"
                    })
                    .on_click(cx.listener(|this, _, _, cx| this.toggle_target(cx))),
            )
            .child(Kbd::new(Keystroke::parse("escape").unwrap()).text_xs())
    }
}
