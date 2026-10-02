pub(crate) mod account;
pub(crate) mod actions;
pub(crate) mod activity;
pub(crate) mod approval;
pub(crate) mod artifacts;
pub(crate) mod auto;
pub(crate) mod browser;
pub(crate) mod busy;
pub(crate) mod capabilities;
pub(crate) mod changes;
pub(crate) mod chrome;
pub(crate) mod complete;
pub(crate) mod composer;
pub(crate) mod files;
pub(crate) mod grain;
pub(crate) mod import;
pub(crate) mod jobs;
pub(crate) mod keys;
pub(crate) mod mcp;
pub(crate) mod memory;
pub(crate) mod messages;
pub(crate) mod messaging;
pub(crate) mod palette;
pub(crate) mod panel;
pub(crate) mod pictures;
pub(crate) mod plugin_page;
pub(crate) mod profiles;
pub(crate) mod quick;
pub(crate) mod schedules;
pub(crate) mod search;
pub(crate) mod sessions;
pub(crate) mod settings;
pub(crate) mod setup;
pub(crate) mod sidebar;
pub(crate) mod skills;
pub(crate) mod update;
pub(crate) mod usage;
pub(crate) mod voice;
pub(crate) mod workflows;

use crate::prelude::*;
use gpui_component::{
    button::{ButtonCustomVariant, ButtonVariant},
    dialog::DialogButtonProps,
};

const CODE_LINE: Pixels = px(18.);

/// Button looks that stay still under the pointer and fill only while
/// pressed. gpui-component works a ghost button's hover out from `secondary`
/// and fills an outline one with `secondary_hover` over the page, so no theme
/// token can hold either still; as custom variants they can.
pub(crate) trait NoHover: ButtonVariants {
    /// No fill and no edge, in place of `ghost`.
    fn plain(self, cx: &App) -> Self {
        self.custom(pressed_only(
            cx.theme().transparent,
            cx.theme().transparent,
            cx,
        ))
    }

    /// A hairline edge on the page colour, in place of `outline`.
    fn framed(self, cx: &App) -> Self {
        self.custom(pressed_only(cx.theme().background, cx.theme().border, cx))
    }
}

impl NoHover for Button {}

/// A custom look whose hover is its resting fill, pressed and selected
/// showing `secondary_active` as an outline button does (a ghost one
/// pressed to a lighter `secondary`).
fn pressed_only(ground: Hsla, edge: Hsla, cx: &App) -> ButtonCustomVariant {
    ButtonCustomVariant::new(cx)
        .color(ground)
        .hover(ground)
        .border(edge)
        .foreground(cx.theme().secondary_foreground)
        .active(cx.theme().secondary_active)
}

/// A page laid out in the chat's column.
pub(crate) trait PageColumn: Styled + Sized {
    /// Centred in the pane, as wide as the chat's column at most and inside
    /// its gutters, so switching between the chat and a page keeps the left
    /// edge still and a wide window widens the margins, not the lines.
    fn page_column(self) -> Self {
        // A width given outright and capped by the pane, as the chat's is,
        // which nothing inside can push wider.
        self.w(COLUMN).max_w_full().mx_auto().px_6()
    }
}

impl<T: Styled> PageColumn for T {}

/// A page's title with its actions at the right, and the line under it
/// saying what the page is for: one heading for every page.
pub(crate) fn page_header(
    title: impl Into<SharedString>,
    blurb: impl Into<SharedString>,
    actions: impl IntoIterator<Item = AnyElement>,
    cx: &App,
) -> Div {
    v_flex()
        .gap_1()
        .child(
            h_flex()
                .items_center()
                .justify_between()
                .gap_4()
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_xl()
                        .font_family(HEADING_FONT)
                        .font_weight(FontWeight::SEMIBOLD)
                        .child(title.into()),
                )
                .child(
                    h_flex()
                        .flex_shrink_0()
                        .gap_1()
                        .items_center()
                        .children(actions),
                ),
        )
        .child(
            div()
                .text_sm()
                .text_color(cx.theme().muted_foreground)
                .child(blurb.into()),
        )
}

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
