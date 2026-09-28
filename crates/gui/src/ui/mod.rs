pub(crate) mod account;
pub(crate) mod actions;
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
use gpui_component::{
    button::{ButtonCustomVariant, ButtonVariant},
    dialog::DialogButtonProps,
};

const CODE_LINE: Pixels = px(18.);

/// Micro labels: the site's 10px `.micro` half a pixel larger, since gpui
/// cannot space the letters out as the site does.
const MICRO: Pixels = px(10.5);

/// Page titles, the dashboard's smallest `.dash-title`.
const PAGE_TITLE: Pixels = px(40.);

/// wordmark.svg's width over its height, 5427 by 785 in its own units.
const WORDMARK_ASPECT: f32 = 5427. / 785.;

/// A small label in DM Mono capitals, the site's `.micro`: sidebar groups,
/// the model over a reply, key hints.
pub(crate) fn micro(text: &str, cx: &App) -> Div {
    div()
        .font_family(data_font(cx))
        .text_size(MICRO)
        .text_color(cx.theme().muted_foreground)
        .child(capitals(text))
}

/// A section label in Barlow Condensed capitals: dialog titles and the heads
/// of settings pages and forms.
pub(crate) fn section(text: &str, cx: &App) -> Div {
    div()
        .font_family(display_font(cx))
        .font_weight(FontWeight::MEDIUM)
        .text_size(px(14.))
        .child(capitals(text))
}

/// The site's display type, light condensed capitals with no leading: page
/// titles and the empty chat's greeting. Only at 28px and up, as weight 200
/// breaks up on a dark page below that.
pub(crate) fn display(text: &str, size: Pixels, cx: &App) -> Div {
    div()
        .font_family(display_font(cx))
        .font_weight(FontWeight::EXTRA_LIGHT)
        .text_size(size)
        .line_height(size)
        .child(capitals(text))
}

/// A page's head, after the dashboard's `.dash-top`: its title over a muted
/// line saying what the page is for.
pub(crate) fn page_header(title: &str, blurb: &'static str, cx: &App) -> Div {
    v_flex()
        .gap_2()
        .child(display(title, PAGE_TITLE, cx))
        .child(
            div()
                .text_sm()
                .text_color(cx.theme().muted_foreground)
                .child(blurb),
        )
}

/// letronna.ai's wordmark, its star and "Letronna" in Bodoni Moda outlines,
/// `height` tall.
pub(crate) fn wordmark(height: Pixels, cx: &App) -> Svg {
    svg()
        .path("icons/wordmark.svg")
        .h(height)
        .w(height * WORDMARK_ASPECT)
        .text_color(cx.theme().foreground)
}

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

/// Words in capitals, raised as they are drawn: gpui has no text-transform,
/// and this way every label stays in sentence case where it is written.
fn capitals(text: &str) -> SharedString {
    text.to_uppercase().into()
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
            .title(section(title, cx))
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

#[cfg(test)]
mod tests {
    use super::capitals;

    #[test]
    fn micro_uppercases_at_draw_time() {
        // Written in sentence case, drawn in capitals: accents and the
        // letters that grow (ß to SS) follow Unicode, numbers and marks stay.
        assert_eq!(capitals("Today"), "TODAY");
        assert_eq!(capitals("Plan · 2 of 5 done"), "PLAN · 2 OF 5 DONE");
        assert_eq!(capitals("gpt-5.1-mini"), "GPT-5.1-MINI");
        assert_eq!(capitals("Résumé straße"), "RÉSUMÉ STRASSE");
    }
}
