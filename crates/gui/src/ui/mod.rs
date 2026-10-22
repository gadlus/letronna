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
pub(crate) mod motion;
pub(crate) mod on_device;
pub(crate) mod pace;
pub(crate) mod palette;
pub(crate) mod panel;
pub(crate) mod pictures;
pub(crate) mod plugin_page;
pub(crate) mod profiles;
pub(crate) mod quick;
pub(crate) mod remember;
pub(crate) mod schedules;
pub(crate) mod search;
pub(crate) mod sessions;
pub(crate) mod settings;
pub(crate) mod setup;
pub(crate) mod sidebar;
pub(crate) mod skills;
pub(crate) mod switcher;
pub(crate) mod update;
pub(crate) mod usage;
pub(crate) mod viewer;
pub(crate) mod voice;
pub(crate) mod wallet;
pub(crate) mod workflows;

use crate::prelude::*;
use gpui_component::{
    button::{ButtonCustomVariant, ButtonVariant},
    dialog::DialogButtonProps,
};

const CODE_LINE: Pixels = px(18.);

/// Button looks that stay still under the pointer and fill only while
/// pressed. gpui-component works a ghost button's hover out from `accent`,
/// so no theme token can hold it still; as a custom variant it can. A button
/// with no variant takes the theme's `button` colours, which the theme holds
/// still on the page with a hairline edge.
pub(crate) trait NoHover: ButtonVariants {
    /// No fill and no edge, in place of `ghost`.
    fn plain(self, cx: &App) -> Self {
        self.custom(pressed_only(cx.theme().transparent, cx))
    }

    /// A hairline edge on the page colour, in place of `outline`.
    fn framed(self, _: &App) -> Self {
        self.with_variant(ButtonVariant::Default)
    }
}

impl NoHover for Button {}

/// A custom look whose hover is its resting fill, pressed and selected
/// showing `secondary_active` as a framed button does (a ghost one pressed
/// to a lighter `secondary`).
fn pressed_only(ground: Hsla, cx: &App) -> ButtonCustomVariant {
    ButtonCustomVariant::new(cx)
        .color(ground)
        .hover(ground)
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
    window.open_alert_dialog(cx, move |dialog, _, cx| {
        let delete = delete.clone();
        dialog
            .title(div().font_family(HEADING_FONT).child(title))
            .width(px(420.))
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
    // Counted as the rows it wraps to: one minified line of thousands of
    // characters is a single line but a screenful of rows.
    let rows = rows(&text, FEWEST_PER_ROW);
    let body = div()
        .text_xs()
        .font_family(MONO_FONT)
        .line_height(CODE_LINE)
        .whitespace_normal()
        .child(text);
    capped(id.into(), body, rows, max, window, cx)
}

/// `body`, `rows` rows of code tall, as it is when it fits in `max`, and in
/// a scroll box of at most `max` when it may not.
fn capped(
    id: ElementId,
    body: Div,
    rows: usize,
    max: Pixels,
    window: &mut Window,
    cx: &mut App,
) -> AnyElement {
    if CODE_LINE * (rows as f32) <= max {
        return body.into_any_element();
    }
    let scroll = scroll_of(&id, window, cx);
    div()
        .id(id)
        .relative()
        // At most `max`, not `max` outright: the row count is a guess that
        // errs long, and a box that holds less shrinks to it.
        .max_h(max)
        .child(
            div()
                .id("code-scroll")
                .occlude()
                .max_h(max)
                .overflow_y_scroll()
                .track_scroll(&scroll)
                .child(body),
        )
        .vertical_scrollbar(&scroll)
        .into_any_element()
}

/// The scroll state of the code block keyed on `id`.
fn scroll_of(id: &ElementId, window: &mut Window, cx: &mut App) -> ScrollHandle {
    window
        .use_keyed_state(id.clone(), cx, |_, _| ScrollHandle::default())
        .read(cx)
        .clone()
}

/// Room kept between a control the keys bring into view and the edge of
/// the page it came in at: a page's own margin.
const SHOWN_MARGIN: Pixels = px(24.);

/// `control`, asking the page scrolled around it to bring it into view on
/// the frame the keys arrive on it (see `follow_keys`). `key` names what
/// remembers whether it had them the frame before, so a page scrolled away
/// from it while it keeps them stays where it was put.
pub(crate) fn shown_on_focus(
    control: Stateful<Div>,
    key: SharedString,
    focus: &FocusHandle,
    window: &mut Window,
    cx: &mut App,
) -> Stateful<Div> {
    let had = window.use_keyed_state(key, cx, |_, _| false);
    let has = focus.is_focused(window);
    let arrived = has && !*had.read(cx);
    had.update(cx, |had, _| *had = has);
    control.when(arrived, |control| {
        control.child(
            canvas(
                |bounds, window, _| window.request_autoscroll(bounds),
                |_, _, _, _| {},
            )
            .absolute()
            .size_full(),
        )
    })
}

/// What a page scrolled by `scroll` does once its controls are laid out:
/// brings into view the one the keys just arrived on, from the next frame.
pub(crate) fn follow_keys(
    scroll: &ScrollHandle,
) -> impl Fn(Vec<Bounds<Pixels>>, &mut Window, &mut App) + 'static {
    let scroll = scroll.clone();
    move |_, window, _| {
        let Some(control) = window.take_autoscroll() else {
            return;
        };
        let page = scroll.bounds();
        let mut offset = scroll.offset();
        let above = page.top() - (control.top() - SHOWN_MARGIN);
        let below = control.bottom() + SHOWN_MARGIN - page.bottom();
        if above > px(0.) {
            offset.y += above;
        } else if below > px(0.) {
            offset.y -= below;
        } else {
            return;
        }
        scroll.set_offset(offset);
        window.request_animation_frame();
    }
}

/// Characters a row of wrapped code is taken to hold when it is not known
/// how wide the code is drawn: fewer than the narrowest chat column holds,
/// so a long line is never taken for fewer rows than it wraps to.
const FEWEST_PER_ROW: usize = 40;

/// Rows of text a preview shows at rest, above its `⋯ N more lines` row.
pub(crate) const PREVIEW_ROWS: usize = 8;

/// Characters a preview paints at rest at most: more than eight rows of the
/// widest column hold, so a line of megabytes is never laid out to be cut.
const PREVIEW_CHARS: usize = 2_000;

/// Lines an opened preview paints at most; copying still takes all of it.
const OPEN_LINES: usize = 400;

/// The least an opened preview grows to before it scrolls, about twenty
/// rows; a tall window gives it 40% of its height instead.
const OPEN_MAX: Pixels = px(360.);

/// How tall the row under a preview is: shorter than a tool row's 26px,
/// as it belongs to the text above it.
const MORE_ROW: Pixels = px(22.);

/// What opens or closes a preview: the caller's, which keeps the state.
pub(crate) type Toggle = std::rc::Rc<dyn Fn(&mut Window, &mut App)>;

/// Which end of a long text its preview keeps.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum Cut {
    /// The start, as a file or a page is read.
    Head,
    /// The end, where a command's errors and summary are.
    Tail,
}

/// How a preview colours its lines.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum Ink {
    /// In the colour around it.
    Plain,
    /// The line at the cut's edge in `danger` and the rest muted, so a
    /// failure's own words stand out of what led to them.
    Failed,
    /// Lines that start with `+` green and with `-` red, as a diff reads.
    Diff,
}

/// What a preview shows of a text too long to show whole, and how many
/// rows of it are left out.
#[derive(Debug, PartialEq)]
pub(crate) struct Rest {
    pub(crate) shown: String,
    pub(crate) hidden: usize,
}

/// How many rows `text` wraps to with `per_row` characters to a row.
pub(crate) fn rows(text: &str, per_row: usize) -> usize {
    let per_row = per_row.max(1);
    text.lines()
        .map(|line| line.chars().count().div_ceil(per_row).max(1))
        .sum()
}

/// What `text` shows at rest when it wraps to more than `limit` rows of
/// `per_row` characters, the rows at its `cut` end, or `None` when it fits.
pub(crate) fn rest(text: &str, cut: Cut, per_row: usize, limit: usize) -> Option<Rest> {
    let per_row = per_row.max(1);
    let text = text.trim_end();
    let total = rows(text, per_row);
    if total <= limit {
        return None;
    }
    let mut kept: Vec<String> = Vec::new();
    let mut used = 0;
    let lines: Box<dyn Iterator<Item = &str>> = match cut {
        Cut::Head => Box::new(text.lines()),
        Cut::Tail => Box::new(text.lines().rev()),
    };
    for line in lines {
        let chars = line.chars().count();
        let tall = chars.div_ceil(per_row).max(1);
        if used + tall <= limit {
            kept.push(line.to_owned());
            used += tall;
            continue;
        }
        // Part of the line that crosses the limit, from the kept end.
        let room = (limit - used) * per_row;
        if room > 0 {
            kept.push(match cut {
                Cut::Head => line.chars().take(room).collect(),
                Cut::Tail => line.chars().skip(chars - room).collect(),
            });
            used = limit;
        }
        break;
    }
    if cut == Cut::Tail {
        kept.reverse();
    }
    let mut shown = kept.join("\n");
    if shown.len() > PREVIEW_CHARS {
        shown = match cut {
            Cut::Head => shown.chars().take(PREVIEW_CHARS).collect(),
            Cut::Tail => {
                let skip = shown.chars().count().saturating_sub(PREVIEW_CHARS);
                shown.chars().skip(skip).collect()
            }
        };
    }
    Some(Rest {
        shown,
        hidden: total - used,
    })
}

/// What the row under a closed preview says.
pub(crate) fn more_lines(hidden: usize) -> String {
    match hidden {
        1 => "⋯ 1 more line".to_owned(),
        n => format!("⋯ {n} more lines"),
    }
}

/// The words on the row over a tail, whose hidden lines came before it.
fn earlier_lines(hidden: usize) -> String {
    match hidden {
        1 => "⋯ 1 earlier line".to_owned(),
        n => format!("⋯ {n} earlier lines"),
    }
}

/// At most `OPEN_LINES` lines of `text` from its `cut` end, with a line
/// saying how many more there are at the other.
fn clamped(text: &str, cut: Cut) -> String {
    let text = text.trim_end();
    let count = text.lines().count();
    if count <= OPEN_LINES {
        return text.to_owned();
    }
    let more = count - OPEN_LINES;
    match cut {
        Cut::Head => {
            let kept: Vec<&str> = text.lines().take(OPEN_LINES).collect();
            format!("{}\n⋯ {more} more lines", kept.join("\n"))
        }
        Cut::Tail => {
            let kept: Vec<&str> = text.lines().skip(more).collect();
            format!("⋯ {more} earlier lines\n{}", kept.join("\n"))
        }
    }
}

/// Lines of code in the colours `ink` gives them.
fn inked(text: String, cut: Cut, ink: Ink, cx: &App) -> Div {
    let body = div()
        .text_xs()
        .font_family(MONO_FONT)
        .line_height(CODE_LINE)
        .whitespace_normal()
        .min_w_0();
    let theme = cx.theme();
    match ink {
        Ink::Plain => body.child(text),
        Ink::Failed => {
            let (rest, edge) = match cut {
                Cut::Head => {
                    let (first, rest) = text.split_once('\n').unwrap_or((&text, ""));
                    (rest.to_owned(), first.to_owned())
                }
                Cut::Tail => {
                    let (rest, last) = text.rsplit_once('\n').unwrap_or(("", &text));
                    (rest.to_owned(), last.to_owned())
                }
            };
            let edge = div().text_color(theme.danger).child(edge);
            let rest =
                (!rest.is_empty()).then(|| div().text_color(theme.muted_foreground).child(rest));
            match cut {
                Cut::Head => body.child(edge).children(rest),
                Cut::Tail => body.children(rest).child(edge),
            }
        }
        Ink::Diff => body.children(text.lines().map(|line| {
            let color = if line.starts_with('+') {
                theme.green
            } else if line.starts_with('-') {
                theme.danger
            } else {
                theme.muted_foreground
            };
            div().text_color(color).child(line.to_owned())
        })),
    }
}

/// A long text kept small: `PREVIEW_ROWS` rows from its `cut` end over a
/// row that opens all of it in a scroll box, `width` being how wide it is
/// drawn and `open` the caller's state, which `toggle` flips.
#[allow(clippy::too_many_arguments)]
pub(crate) fn preview_block(
    id: ElementId,
    text: &str,
    cut: Cut,
    ink: Ink,
    width: Pixels,
    open: bool,
    toggle: Toggle,
    window: &mut Window,
    cx: &mut App,
) -> AnyElement {
    // Its rows are text_xs.
    let per_row = per_row(width, window.rem_size() * 0.75, window);
    let Some(rest) = rest(text, cut, per_row, PREVIEW_ROWS) else {
        return inked(text.trim_end().to_owned(), cut, ink, cx).into_any_element();
    };
    // Held while the preview shows, so opening a command's output can
    // start it at the end the preview showed.
    let scroll = scroll_of(&id, window, cx);
    if !open {
        let toggle_it = toggle.clone();
        let open_it: Toggle = std::rc::Rc::new(move |window: &mut Window, cx: &mut App| {
            if cut == Cut::Tail {
                scroll.scroll_to_bottom();
            }
            toggle_it(window, cx)
        });
        let shown = div()
            // Clipped to the rows, as a long line can still wrap past the
            // guess; a tail is held to the bottom of its box.
            .flex()
            .flex_col()
            .when(cut == Cut::Tail, |this| this.justify_end())
            .max_h(CODE_LINE * PREVIEW_ROWS as f32)
            .overflow_hidden()
            .child(inked(rest.shown, cut, ink, cx));
        // The row stands where the hidden lines are: over a tail, under a head.
        let more = more_row(
            ElementId::NamedChild(id.into(), "more".into()),
            match cut {
                Cut::Tail => earlier_lines(rest.hidden),
                Cut::Head => more_lines(rest.hidden),
            },
            false,
            open_it,
            window,
            cx,
        );
        let column = v_flex().w_full();
        return match cut {
            Cut::Tail => column.child(more).child(shown),
            Cut::Head => column.child(shown).child(more),
        }
        .into_any_element();
    }
    let whole = clamped(text, cut);
    let rows = rows(&whole, per_row);
    // A tall window gets a taller box before it scrolls.
    let max = OPEN_MAX.max(window.viewport_size().height * 0.4);
    v_flex()
        .w_full()
        .child(capped(
            id.clone(),
            inked(whole, cut, ink, cx),
            rows,
            max,
            window,
            cx,
        ))
        .child(more_row(
            ElementId::NamedChild(id.into(), "more".into()),
            "Show less".to_owned(),
            true,
            toggle,
            window,
            cx,
        ))
        .into_any_element()
}

/// How many characters of code at `size` fit on a row `width` wide.
pub(crate) fn per_row(width: Pixels, size: Pixels, window: &Window) -> usize {
    let system = window.text_system();
    let font = system.resolve_font(&font(MONO_FONT));
    let advance = system
        .em_advance(font, size)
        .unwrap_or(size * 0.6)
        .max(px(1.));
    ((width / advance).floor() as usize).max(FEWEST_PER_ROW / 2)
}

/// The row that opens a preview, or closes it when `open`, as a tab stop
/// that Enter or Space toggles as they do a folded run.
pub(crate) fn more_row(
    id: ElementId,
    label: String,
    open: bool,
    toggle: Toggle,
    window: &mut Window,
    cx: &mut App,
) -> impl IntoElement {
    let focus = window
        .use_keyed_state(
            ElementId::NamedChild(id.clone().into(), "focus".into()),
            cx,
            |_, cx| cx.focus_handle().tab_stop(true),
        )
        .read(cx)
        .clone();
    let theme = cx.theme();
    let on_key = toggle.clone();
    h_flex()
        .id(id)
        .track_focus(&focus)
        .key_context("ToolRun")
        .h(MORE_ROW)
        .w_full()
        .min_w_0()
        // The pressed and focused fill reaches a little left of the text,
        // which stays in line with what it opens.
        .ml(px(-4.))
        .px_1()
        .gap_1()
        .items_center()
        .rounded(theme.radius)
        .cursor_pointer()
        .text_xs()
        .font_family(HEADING_FONT)
        .text_color(theme.muted_foreground)
        .focus(|s| s.bg(theme.secondary))
        .active(|s| s.bg(theme.secondary.opacity(0.5)))
        .child(label)
        .child(
            Icon::new(if open {
                IconName::ChevronUp
            } else {
                IconName::ChevronDown
            })
            .size_3(),
        )
        // As on a folded run: a press leaves the keys where they were, and
        // the keyboard's own click on Enter or Space is left to ToggleRun.
        .on_any_mouse_down(|_, window, _| window.prevent_default())
        .on_click(move |event: &ClickEvent, window, cx| {
            if !event.is_keyboard() {
                toggle(window, cx);
            }
        })
        .on_action(move |_: &crate::ui::messages::ToggleRun, window, cx| on_key(window, cx))
}

/// A small Copy button that stays still under the pointer, unlike
/// gpui-component's `Clipboard`: a press copies what `text` gives then and
/// says so.
pub(crate) fn copy_button(
    id: impl Into<ElementId>,
    text: impl Fn(&mut App) -> String + 'static,
    cx: &App,
) -> Button {
    Button::new(id)
        .plain(cx)
        .xsmall()
        .icon(IconName::Copy)
        .tooltip("Copy")
        .on_click(move |_, window, cx| {
            cx.stop_propagation();
            let text = text(cx);
            cx.write_to_clipboard(ClipboardItem::new_string(text));
            window.push_notification(
                gpui_component::notification::Notification::success("Copied."),
                cx,
            );
        })
}

#[cfg(test)]
mod tests {
    // Not `use super::*`: the prelude's gpui `test` macro would shadow the
    // built-in attribute.
    use super::{Cut, Rest, clamped, earlier_lines, follow_keys, more_lines, rest, rows};
    use super::{capabilities, settings::KeyedSwitch};
    use gpui::{
        AppContext as _, Context, InteractiveElement as _, IntoElement, ParentElement as _, Render,
        ScrollHandle, SharedString, StatefulInteractiveElement as _, Styled as _, TestAppContext,
        VisualTestContext, Window, div, point, px,
    };
    use gpui_component::Root;

    #[test]
    fn a_long_line_still_counts_as_many_rows() {
        assert_eq!(rows("short\nlines", 40), 2);
        assert_eq!(rows(&"x".repeat(8_000), 100), 80);
        assert_eq!(rows("", 40), 0);
        assert_eq!(rows("a\n\nb", 40), 3);
    }

    #[test]
    fn text_that_fits_has_no_preview() {
        assert_eq!(rest("one\ntwo\nthree", Cut::Head, 80, 8), None);
        // Trailing blank lines do not push it over.
        assert_eq!(rest("1\n2\n3\n4\n5\n6\n7\n8\n\n\n", Cut::Tail, 80, 8), None);
    }

    #[test]
    fn a_head_keeps_the_first_rows_and_counts_the_rest() {
        let text: Vec<String> = (1..=20).map(|n| format!("line {n}")).collect();
        let shown = rest(&text.join("\n"), Cut::Head, 80, 8);
        assert_eq!(
            shown,
            Some(Rest {
                shown: text[..8].join("\n"),
                hidden: 12
            })
        );
    }

    #[test]
    fn a_tail_keeps_the_last_rows_in_order() {
        let text: Vec<String> = (1..=20).map(|n| format!("line {n}")).collect();
        let shown = rest(&text.join("\n"), Cut::Tail, 80, 8);
        assert_eq!(
            shown,
            Some(Rest {
                shown: text[12..].join("\n"),
                hidden: 12
            })
        );
    }

    #[test]
    fn a_minified_line_is_cut_to_the_rows_it_may_take() {
        let line = "y".repeat(8_000);
        let Some(head) = rest(&line, Cut::Head, 100, 8) else {
            panic!("a line of 80 rows fits in 8");
        };
        assert_eq!(head.shown.len(), 800);
        assert_eq!(head.hidden, 72);
        let Some(tail) = rest(&format!("start\n{line}"), Cut::Tail, 100, 8) else {
            panic!("a line of 80 rows fits in 8");
        };
        assert_eq!(tail.shown.len(), 800);
        assert!(!tail.shown.contains("start"));
    }

    #[test]
    fn the_more_row_counts_in_words() {
        assert_eq!(more_lines(1), "⋯ 1 more line");
        assert_eq!(more_lines(42), "⋯ 42 more lines");
        assert_eq!(earlier_lines(1), "⋯ 1 earlier line");
        assert_eq!(earlier_lines(42), "⋯ 42 earlier lines");
    }

    #[test]
    fn an_opened_tail_keeps_its_end_when_clamped() {
        let text: Vec<String> = (1..=450).map(|n| n.to_string()).collect();
        let tail = clamped(&text.join("\n"), Cut::Tail);
        assert!(tail.starts_with("⋯ 50 earlier lines\n51\n"));
        assert!(tail.ends_with("\n450"));
        let head = clamped(&text.join("\n"), Cut::Head);
        assert!(head.starts_with("1\n2\n"));
        assert!(head.ends_with("400\n⋯ 50 more lines"));
    }

    /// Ten switches, a settings one and a capabilities one in turn, on a
    /// page that shows two and a bit.
    struct LongPage {
        scroll: ScrollHandle,
    }

    impl Render for LongPage {
        fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            let rows: Vec<_> = (0..10)
                .map(|n| {
                    let id = SharedString::from(format!("row-{n}"));
                    let control = if n % 2 == 0 {
                        KeyedSwitch::new(id).into_any_element()
                    } else {
                        capabilities::switch(id, false, window, cx, |_, _, _| {}).into_any_element()
                    };
                    div().py_2().child(control)
                })
                .collect();
            div()
                .on_children_prepainted(follow_keys(&self.scroll))
                .id("page")
                .track_scroll(&self.scroll)
                .h(px(100.))
                .overflow_y_scroll()
                .children(rows)
        }
    }

    /// Whether row `n` is all in sight, once the frame after this one, where
    /// the page moves to it, is drawn.
    fn in_sight(n: usize, scroll: &ScrollHandle, cx: &mut VisualTestContext) -> bool {
        cx.update(|window, cx| window.simulate_next_frame(cx));
        cx.run_until_parked();
        let (page, row, y) = (
            scroll.bounds(),
            scroll.bounds_for_item(n).unwrap(),
            scroll.offset().y,
        );
        row.top() + y >= page.top() && row.bottom() + y <= page.bottom()
    }

    #[gpui::test]
    fn a_page_scrolls_to_the_switch_the_keys_move_to_and_no_further(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        let page = cx.new(|_| LongPage {
            scroll: ScrollHandle::new(),
        });
        let scroll = cx.read(|cx| page.read(cx).scroll.clone());
        let (_, cx) = cx.add_window_view(|window, cx| Root::new(page.clone(), window, cx));
        // The first row, then down past the fold to the fourth, a
        // capabilities switch.
        cx.update(|window, cx| window.focus_next(cx));
        assert!(in_sight(0, &scroll, cx));
        assert_eq!(scroll.offset().y, px(0.));
        cx.simulate_keystrokes("tab tab tab");
        assert!(in_sight(3, &scroll, cx));
        assert!(scroll.offset().y < px(0.));
        // Scrolled back up by hand, the page stays there while the keys do.
        scroll.set_offset(point(px(0.), px(0.)));
        page.update(cx, |_, cx| cx.notify());
        assert!(!in_sight(3, &scroll, cx));
        // On to a settings switch, then back up to the top.
        cx.simulate_keystrokes("tab");
        assert!(in_sight(4, &scroll, cx));
        for n in (0..4).rev() {
            cx.simulate_keystrokes("shift-tab");
            assert!(in_sight(n, &scroll, cx));
        }
        assert_eq!(scroll.offset().y, px(0.));
    }
}
