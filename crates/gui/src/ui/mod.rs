pub(crate) mod account;
pub(crate) mod approval;
pub(crate) mod artifacts;
pub(crate) mod browser;
pub(crate) mod chrome;
pub(crate) mod composer;
pub(crate) mod messages;
pub(crate) mod messaging;
pub(crate) mod schedules;
pub(crate) mod sessions;
pub(crate) mod settings;
pub(crate) mod sidebar;
pub(crate) mod skills;
pub(crate) mod update;

use crate::prelude::*;
use gpui_component::{button::ButtonVariant, dialog::DialogButtonProps};

const CODE_LINE: Pixels = px(18.);

/// Ask before something is removed for good. The button says what it does
/// and is red, where a plain "OK" read as agreeing to carry on.
pub(crate) fn confirm_delete(
    title: &'static str,
    body: String,
    window: &mut Window,
    cx: &mut App,
    delete: impl Fn(&mut Window, &mut App) + 'static,
) {
    let delete = std::rc::Rc::new(delete);
    window.open_dialog(cx, move |dialog, _, cx| {
        let delete = delete.clone();
        dialog
            .title(div().font_family(HEADING_FONT).child(title))
            .w(px(420.))
            .confirm()
            .button_props(
                DialogButtonProps::default()
                    .ok_text("Delete")
                    .ok_variant(ButtonVariant::Danger),
            )
            .child(
                div()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child(body.clone()),
            )
            .on_ok(move |_, window, cx| {
                delete(window, cx);
                true
            })
    });
}

/// Monospaced output that turns into a scroll box only once it is taller than
/// `max`. A short command then takes the room it needs instead of reserving a
/// tall panel, and a long one clips instead of painting over the chat.
///
/// Every block needs its own scroll state, keyed on `id`. The convenience
/// `overflow_y_scrollbar()` keys on the call site instead, so all of them
/// would share one handle and fight over it.
pub(crate) fn code_block(
    id: impl Into<ElementId>,
    text: String,
    max: Pixels,
    window: &mut Window,
    cx: &mut App,
) -> impl IntoElement {
    let id = id.into();
    let body = div()
        .text_xs()
        .font_family(MONO_FONT)
        .line_height(CODE_LINE)
        .whitespace_normal()
        .child(text.clone());
    if CODE_LINE * (text.lines().count() as f32) <= max {
        return body.into_any_element();
    }
    let scroll = window
        .use_keyed_state(id.clone(), cx, |_, _| ScrollHandle::default())
        .read(cx)
        .clone();
    div()
        .id(id)
        .relative()
        // A definite height, so the scroll area inside has something to
        // resolve `size_full` against.
        .h(max)
        .child(
            div()
                .id("code-scroll")
                .occlude()
                .size_full()
                .overflow_y_scroll()
                .track_scroll(&scroll)
                .child(body),
        )
        .vertical_scrollbar(&scroll)
        .into_any_element()
}
