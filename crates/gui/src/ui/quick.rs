//! The quick entry bar: a second, small window over whatever the user is
//! doing, opened by the global shortcut or the tray's "Quick entry" item.
//! It is a plain gpui window in this same process, so it reaches the same
//! [`Chat`] entity the main window renders -- there is nothing to boot and
//! nothing else to keep in sync. After a send the bar grows to show the end
//! of the reply as it comes in, from that same entity's changes, and goes
//! back to the bar alone a moment after the turn is over.

use crate::prelude::*;
use crate::ui::pictures;
use gpui_component::input::Paste;
use std::time::Duration;

actions!(quick, [Hide, ToggleTarget, OpenMain]);

const CONTEXT: &str = "Quick";
const SIZE: Size<Pixels> = size(px(560.), px(56.));
/// How many of the reply's last lines show with the bar while it runs.
const LINES: usize = 3;
/// The most of a reply's end that is painted: three lines at the bar's width
/// hold some 230 characters, so this leaves room for narrow ones, and a
/// minified line of thousands lays out no more than this.
const PAINTED: usize = 400;
/// What the reply adds to the window's height: a 1px rule, three 20px lines
/// with 8px above and below, and a 24px row naming the session.
const BAND: Pixels = px(101.);
/// The reply's part of the band: three lines with 8px above and below.
const TEXT: Pixels = px(76.);
/// One line of the reply, set rather than left to the font, so three fill
/// `TEXT` exactly.
const LINE: Pixels = px(20.);
/// The band's row naming the session, with the way into the main window.
const ROW: Pixels = px(24.);
/// How long the answer stays up once its turn is over, so its last words can
/// be read before the bar shrinks back.
const HOLD: Duration = Duration::from_millis(1000);
/// macOS grows a window from its bottom edge, so there the reply goes above
/// the bar; elsewhere the window grows down and it goes below. Either way the
/// bar the user is typing in stays where it was.
const ABOVE: bool = cfg!(target_os = "macos");

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
        KeyBinding::new("secondary-enter", OpenMain, Some(CONTEXT)),
    ]);
}

/// Open the bar, or dismiss it if the shortcut was pressed again while it is
/// already up. Called from the global hotkey and the tray's fallback item.
pub(crate) fn toggle(cx: &mut App) {
    if cx.has_global::<Open>() {
        let Open(handle) = cx.remove_global::<Open>();
        // A bar the system closed itself leaves its handle behind; that press
        // then opens a new one rather than being spent on a window not there.
        if handle
            .update(cx, |_, window, _| window.remove_window())
            .is_ok()
        {
            return;
        }
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
    // gpui on Windows opens a window without the keys, whatever `focus`
    // says, so typing would go on in the app behind the bar. The shortcut's
    // press lets this process take the front, and the bar asks for it.
    let _ = handle.update(cx, |_, window, _| window.activate_window());
    cx.set_global(Open(handle));
}

struct QuickEntry {
    main: Entity<Chat>,
    input: Entity<InputState>,
    new_session: bool,
    /// Pictures pasted into the bar, waiting to go with the next send.
    images: Vec<PathBuf>,
    /// Why the last send did not go, shown until the next edit or send.
    error: Option<String>,
    /// The turn the bar sent, shown with it until a moment after it ends.
    watch: Option<Watch>,
    /// Shrinks the bar back once the ended turn's answer has had its `HOLD`.
    shrink: Option<Task<()>>,
    _subscriptions: Vec<Subscription>,
}

/// A turn the bar sent, followed as the main window's chat changes.
struct Watch {
    session: u64,
    /// How many messages the session held with the prompt in; those after
    /// are this turn's.
    from: usize,
    /// What the band shows of it now.
    glance: Glance,
}

/// What the band shows of a turn: its session, how far along it is, and the
/// last lines of its reply.
#[derive(Clone, Default, PartialEq)]
struct Glance {
    title: SharedString,
    stage: Stage,
    lines: Vec<String>,
}

/// How far along a turn the bar sent is.
#[derive(Clone, Copy, Default, PartialEq)]
enum Stage {
    #[default]
    Running,
    /// Stopped on a card in the main window, waiting for the user's answer.
    Waiting,
    Done,
    Failed,
}

impl Watch {
    /// The turn as the chat has it now: the reply coming in, or else the
    /// newest one it kept, a failure included.
    fn glance(&self, chat: &Chat) -> Glance {
        let running = chat.busy && chat.streaming_id == Some(self.session);
        let live = chat
            .turn
            .as_ref()
            .filter(|_| running)
            .map(|turn| turn.live.content.as_str())
            .filter(|text| !text.trim().is_empty());
        let Some(ix) = chat.store.index_of(self.session) else {
            return Glance {
                stage: Stage::Done,
                ..Glance::default()
            };
        };
        let conversation = &chat.store.conversations[ix];
        let kept = conversation
            .messages
            .get(self.from..)
            .unwrap_or_default()
            .iter()
            .rev()
            .find(|message| {
                (message.role == "assistant" || message.role == letronna_core::failure::ROLE)
                    && !message.content.trim().is_empty()
            });
        let failed = kept.is_some_and(|message| message.role == letronna_core::failure::ROLE);
        let stage = match () {
            _ if running && chat.asking.is_some() => Stage::Waiting,
            _ if running => Stage::Running,
            _ if failed => Stage::Failed,
            _ => Stage::Done,
        };
        let text = live
            .or(kept.map(|message| message.content.as_str()))
            .unwrap_or_default();
        Glance {
            title: conversation.title.clone().into(),
            stage,
            lines: tail(text, LINES, PAINTED),
        }
    }
}

/// The last `lines` lines of `text` that are not blank, at most `chars`
/// characters of them: the newest line is cut at its start, with an
/// ellipsis, and an earlier one that does not fit is left out.
fn tail(text: &str, lines: usize, chars: usize) -> Vec<String> {
    // Only the end is read, so a long reply costs what a short one does;
    // four bytes are the most a character takes.
    let mut from = text.len().saturating_sub(chars * 4);
    while !text.is_char_boundary(from) {
        from += 1;
    }
    let partial = from > 0 && !text[..from].ends_with('\n');
    let end: Vec<&str> = text[from..].lines().collect();
    let mut budget = chars;
    let mut kept = Vec::new();
    for (i, line) in end.iter().enumerate().rev() {
        let line = line.trim_end();
        if line.trim().is_empty() {
            continue;
        }
        if kept.len() == lines || budget == 0 {
            break;
        }
        let count = line.chars().count();
        if count <= budget && !(i == 0 && partial) {
            kept.push(line.to_owned());
            budget -= count;
            continue;
        }
        // A sliver of an earlier line would say nothing.
        if kept.is_empty() {
            let rest: String = line.chars().skip(count.saturating_sub(budget)).collect();
            kept.push(format!("\u{2026}{}", rest.trim_start()));
        }
        break;
    }
    kept.reverse();
    kept
}

impl QuickEntry {
    fn new(main: Entity<Chat>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let input = cx.new(|cx| InputState::new(window, cx).placeholder("Ask Letronna\u{2026}"));
        input.update(cx, |input, cx| input.focus(window, cx));
        let subscriptions = vec![
            cx.subscribe_in(&input, window, |this, _, event, window, cx| match event {
                // The secondary one is `OpenMain`, bound in this context.
                InputEvent::PressEnter { secondary: false } => this.send(window, cx),
                InputEvent::Change if this.error.take().is_some() => cx.notify(),
                _ => {}
            }),
            cx.observe_in(&main, window, |this, _, window, cx| {
                this.catch_up(window, cx)
            }),
        ];
        Self {
            main,
            input,
            new_session: false,
            images: Vec::new(),
            error: None,
            watch: None,
            shrink: None,
            _subscriptions: subscriptions,
        }
    }

    fn send(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let prompt = self.input.read(cx).value().trim().to_owned();
        if prompt.is_empty() && self.images.is_empty() {
            return;
        }
        let images = self.images.clone();
        let new_session = self.new_session;
        let sent = self.main.update(cx, |chat, cx| {
            chat.quick_dispatch(prompt, images, new_session, cx)
        });
        match sent {
            Ok(()) => self.watch_reply(window, cx),
            // Kept as typed, so nothing is lost to a reply still running.
            Err(why) => {
                self.error = Some(why);
                cx.notify();
            }
        }
    }

    /// Clear the bar for the next prompt and grow it to show the reply to
    /// the one just sent, in the session it went to.
    fn watch_reply(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.input
            .update(cx, |input, cx| input.set_value("", window, cx));
        self.images.clear();
        self.error = None;
        let chat = self.main.read(cx);
        let conversation = &chat.store.conversations[chat.store.active];
        self.watch = Some(Watch {
            session: conversation.id,
            from: conversation.messages.len(),
            glance: Glance::default(),
        });
        self.shrink = None;
        window.resize(size(SIZE.width, SIZE.height + BAND));
        self.catch_up(window, cx);
        cx.notify();
    }

    /// Bring the band up to date after a change to the chat, and shrink the
    /// bar a moment after the turn is over. Nothing is read without a turn.
    fn catch_up(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(watch) = &self.watch else {
            return;
        };
        let glance = watch.glance(self.main.read(cx));
        if matches!(glance.stage, Stage::Running | Stage::Waiting) {
            // A follow-up queued in the main window went on in the session.
            self.shrink = None;
        } else if self.shrink.is_none() {
            self.shrink = Some(cx.spawn_in(window, async move |this, cx| {
                cx.background_executor().timer(HOLD).await;
                let _ = this.update_in(cx, |this, window, cx| this.shrink(window, cx));
            }));
        }
        if let Some(watch) = &mut self.watch
            && watch.glance != glance
        {
            watch.glance = glance;
            cx.notify();
        }
    }

    /// Back to the bar alone, for the next prompt; or out of the way when
    /// the user has gone on to another window, where it would only float.
    fn shrink(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.watch = None;
        if !window.is_window_active() {
            return self.hide(window, cx);
        }
        window.resize(SIZE);
        cx.notify();
    }

    /// Send what is typed, if anything, then bring the main window to the
    /// front on the session the bar is showing, or would send to.
    fn open_main(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let typed = !self.input.read(cx).value().trim().is_empty() || !self.images.is_empty();
        if typed {
            self.send(window, cx);
            if self.error.is_some() {
                return;
            }
        }
        let session = match &self.watch {
            Some(watch) => watch.session,
            None => {
                let chat = self.main.read(cx);
                chat.store.conversations[chat.store.active].id
            }
        };
        let main = self.main.clone();
        self.hide(window, cx);
        cx.defer(move |cx| show_main(main, session, cx));
    }

    fn hide(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // Already gone when the shortcut dismissed the bar in the same breath.
        if cx.has_global::<Open>() {
            cx.remove_global::<Open>();
        }
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

    /// The end of the reply and the session it is in, beside the bar while
    /// a turn it sent runs and for a moment after.
    fn band(&self, window: &Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        let glance = &self.watch.as_ref()?.glance;
        let theme = cx.theme();
        let (color, placeholder) = match glance.stage {
            Stage::Running => (theme.muted_foreground, "Working\u{2026}"),
            Stage::Waiting => (theme.muted_foreground, "A call needs your answer."),
            Stage::Done => (theme.foreground, "Done."),
            Stage::Failed => (theme.danger, "That did not go through."),
        };
        let lines = if glance.lines.is_empty() {
            vec![placeholder.to_owned()]
        } else {
            glance.lines.clone()
        };
        // From the bottom up, so a line wrapping past three rows loses its
        // start rather than its end.
        let text = v_flex()
            .flex_none()
            .h(TEXT)
            .py_2()
            // Under the typed text: the bar's 12px, the 16px icon, its 8px
            // gap and the input's own 12px.
            .pl_12()
            .pr_3()
            .justify_end()
            .overflow_hidden()
            .text_sm()
            .line_height(LINE)
            .text_color(color)
            .children(lines.into_iter().map(|line| div().child(line)));
        let (place, place_color) = match glance.stage {
            Stage::Waiting => (
                format!("\u{201c}{}\u{201d} is waiting on you", glance.title).into(),
                theme.foreground,
            ),
            _ => (glance.title.clone(), theme.muted_foreground),
        };
        let row = h_flex()
            .flex_none()
            .h(ROW)
            .pl_12()
            .pr_2()
            .gap_2()
            .items_center()
            .text_xs()
            .text_color(theme.muted_foreground)
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_color(place_color)
                    .child(place),
            )
            .child(
                Button::new("quick-open")
                    .plain(cx)
                    .xsmall()
                    .label("Open in Letronna")
                    .on_click(cx.listener(|this, _, window, cx| this.open_main(window, cx))),
            )
            .children(Kbd::binding_for_action(&OpenMain, Some(CONTEXT), window).map(Kbd::text_xs));
        let band = v_flex().flex_none().h(BAND).border_color(cx.theme().border);
        Some(
            if ABOVE {
                band.border_b_1().child(row).child(text)
            } else {
                band.border_t_1().child(text).child(row)
            }
            .into_any_element(),
        )
    }
}

/// Bring the main window to the front on `session`, as a click on a
/// notification about it does.
fn show_main(main: Entity<Chat>, session: u64, cx: &mut App) {
    // The bar is a panel that never makes the app the active one on macOS.
    cx.activate(true);
    for handle in cx.windows() {
        let _ = handle.update(cx, |_, window, cx| {
            let shows_main = window
                .root::<Root>()
                .flatten()
                .is_some_and(|root| root.read(cx).view().entity_id() == main.entity_id());
            if !shows_main {
                return;
            }
            crate::platform::window::show(window);
            main.update(cx, |chat, cx| {
                let Some(index) = chat.store.index_of(session) else {
                    return;
                };
                chat.leave_for(index);
                chat.store.active = index;
                chat.error = None;
                chat.page = Page::Chat;
                chat.follow();
                chat.focus_composer(window, cx);
                chat.save();
                cx.notify();
            });
        });
    }
}

impl Render for QuickEntry {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let target_label = if self.new_session {
            "New session"
        } else {
            "Continue last"
        };
        let bar = h_flex()
            .flex_none()
            // The window's height inside its 1px border.
            .h(SIZE.height - px(2.))
            .items_center()
            .gap_2()
            .px_3()
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
            .when_some(self.error.clone(), |row, why| {
                row.child(
                    div()
                        .max_w(px(220.))
                        .truncate()
                        .text_xs()
                        .text_color(cx.theme().danger)
                        .child(why),
                )
            })
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
            .child(Kbd::new(Keystroke::parse("escape").unwrap()).text_xs());
        let mut parts = vec![bar.into_any_element()];
        if let Some(band) = self.band(window, cx) {
            if ABOVE {
                parts.insert(0, band);
            } else {
                parts.push(band);
            }
        }
        v_flex()
            .key_context(CONTEXT)
            .on_action(cx.listener(|this, _: &Hide, window, cx| this.hide(window, cx)))
            .on_action(cx.listener(|this, _: &ToggleTarget, _, cx| this.toggle_target(cx)))
            .on_action(cx.listener(|this, _: &OpenMain, window, cx| this.open_main(window, cx)))
            .capture_action(cx.listener(Self::paste_image))
            .size_full()
            // Until the window has grown or shrunk to match, the bar is what
            // stays in view: the window keeps the edge it grows away from.
            .when(ABOVE, |column| column.justify_end())
            .rounded(cx.theme().radius_lg)
            .border_1()
            .border_color(cx.theme().border)
            .bg(cx.theme().background)
            .children(parts)
    }
}

#[cfg(test)]
mod tests {
    // Not `use super::*`: the prelude re-exports gpui, whose `test` macro
    // would shadow the built-in attribute.
    use super::tail;

    #[test]
    fn a_short_reply_shows_whole() {
        assert_eq!(tail("Done.", 3, 400), ["Done."]);
    }

    #[test]
    fn only_the_last_lines_that_say_something_show() {
        let reply = "one\ntwo\n\nthree\n   \nfour\n\n";
        assert_eq!(tail(reply, 3, 400), ["two", "three", "four"]);
    }

    #[test]
    fn a_long_line_loses_its_start_not_its_end() {
        let line = format!("{}the end", "x".repeat(8_000));
        let shown = tail(&line, 3, 400);
        assert_eq!(shown.len(), 1);
        assert!(shown[0].starts_with('\u{2026}'));
        assert!(shown[0].ends_with("the end"));
        assert_eq!(shown[0].chars().count(), 401);
    }

    #[test]
    fn an_earlier_line_that_does_not_fit_is_left_out() {
        let reply = format!("first\nsecond\n{}", "y".repeat(398));
        assert_eq!(tail(&reply, 3, 400), ["y".repeat(398)]);
    }

    #[test]
    fn a_cut_never_splits_a_character() {
        let reply = "\u{00e9}".repeat(5_000);
        let shown = tail(&reply, 3, 400);
        assert_eq!(shown, [format!("\u{2026}{}", "\u{00e9}".repeat(400))]);
    }

    #[test]
    fn nothing_said_shows_nothing() {
        assert!(tail("", 3, 400).is_empty());
        assert!(tail("\n \n", 3, 400).is_empty());
    }
}
