//! The parts a list in the window is drawn with: the row, the mark that says
//! how what it lists is doing, the tag and the chip beside a name, the
//! segments that choose what is shown, a group's label, and what a page shows
//! while it lists nothing.
//!
//! One row for everything listed, so a server, a plugin and a skill read
//! alike: how it is doing, its name, what kind of thing it is, one line in
//! words, the one button that puts it right, and its switch. Whatever else
//! can be done with it is under the row, shown when it is pressed.
//!
//! A state is never told by colour alone: each mark has a shape of its own,
//! and the row's line says it in words.

use crate::prelude::*;
use gpui_component::Selectable;
use std::rc::Rc;

/// How tall a row is at least: two lines of text and the room around them.
const ROW_HEIGHT: Pixels = px(52.);
/// How wide the place a mark is drawn in is, whatever its shape, so the
/// names under one another start at the same edge.
const MARK_PLACE: Pixels = px(14.);

/// How something a row lists is doing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Mark {
    /// Connected, running, in place: a green dot.
    Fine,
    /// Waiting for the user, to sign in or to allow it: an amber triangle.
    NeedsYou,
    /// Failed: a red square.
    Failed,
    /// Starting or being fetched: a dot in the colour of a link.
    Working,
    /// Switched off: a ring.
    Off,
}

/// The shape and colour `mark` is drawn in, in a place as wide as any.
pub(crate) fn mark(mark: Mark, cx: &App) -> Div {
    let theme = cx.theme();
    let dot = |colour: Hsla| div().size(px(8.)).rounded_full().bg(colour);
    let shape = match mark {
        Mark::Fine => dot(theme.green).into_any_element(),
        Mark::Working => dot(theme.link).into_any_element(),
        Mark::Failed => div()
            .size(px(8.))
            .rounded(px(1.))
            .bg(theme.danger)
            .into_any_element(),
        Mark::NeedsYou => Icon::new(AppIcon::Alert)
            .size(px(13.))
            .text_color(theme.warning)
            .into_any_element(),
        Mark::Off => div()
            .size(px(8.))
            .rounded_full()
            .border_1()
            .border_color(theme.muted_foreground)
            .into_any_element(),
    };
    div()
        .w(MARK_PLACE)
        .flex_shrink_0()
        .flex()
        .items_center()
        .justify_center()
        .child(shape)
}

/// What kind of thing a row is, in a hairline box beside its name: how a
/// server is reached, a version, "Built in". Never pressed.
pub(crate) fn tag(text: impl Into<SharedString>, cx: &App) -> Div {
    div()
        .flex_shrink_0()
        .px_1p5()
        .rounded(cx.theme().radius)
        .border_1()
        .border_color(cx.theme().border)
        .text_xs()
        .text_color(cx.theme().muted_foreground)
        .child(text.into())
}

/// One of what something brings or is given, filled where a tag is edged: a
/// variable's name, "1 tool". Never pressed.
pub(crate) fn chip(text: impl Into<SharedString>, cx: &App) -> Div {
    div()
        .flex_shrink_0()
        .px_1p5()
        .rounded(cx.theme().radius)
        .bg(cx.theme().secondary)
        .text_xs()
        .text_color(cx.theme().muted_foreground)
        .child(text.into())
}

/// The label over a group of rows, with how many it holds.
pub(crate) fn group_label(title: &str, count: usize, cx: &App) -> Div {
    div()
        .text_xs()
        .font_family(HEADING_FONT)
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(cx.theme().muted_foreground)
        .child(format!("{} · {count}", title.to_uppercase()))
}

/// Rows in one hairline box, a hairline between each and the next.
pub(crate) fn list(rows: impl IntoIterator<Item = AnyElement>, cx: &App) -> Div {
    let edge = cx.theme().border;
    let rows = rows.into_iter().enumerate().map(move |(at, row)| {
        div()
            .when(at > 0, |this| this.border_t_1().border_color(edge))
            .child(row)
    });
    v_flex()
        .rounded(cx.theme().radius_lg)
        .border_1()
        .border_color(edge)
        .overflow_hidden()
        .children(rows)
}

/// One of a row of segments: what it is called, how many it holds if that
/// is worth saying, whether it is the one picked, and what picking it does.
pub(crate) struct Segment {
    pub(crate) label: &'static str,
    pub(crate) count: Option<usize>,
    pub(crate) picked: bool,
    pub(crate) pick: Press,
}

/// Segments that choose which of a few things is shown: side by side in one
/// hairline box, the picked one filled. Each is a button, so Tab reaches it.
pub(crate) fn segments(id: &SharedString, segments: Vec<Segment>, cx: &App) -> Div {
    let edge = cx.theme().border;
    let buttons = segments.into_iter().enumerate().map(|(at, segment)| {
        let label = match segment.count {
            Some(count) => format!("{} {count}", segment.label),
            None => segment.label.to_owned(),
        };
        let pick = segment.pick;
        div()
            .when(at > 0, |this| this.border_l_1().border_color(edge))
            .child(
                Button::new(SharedString::from(format!("{id}-{at}")))
                    .plain(cx)
                    .xsmall()
                    .selected(segment.picked)
                    .label(label)
                    .on_click(move |event, window, cx| pick(event, window, cx)),
            )
    });
    h_flex()
        .flex_shrink_0()
        .rounded(cx.theme().radius)
        .border_1()
        .border_color(edge)
        .overflow_hidden()
        .children(buttons)
}

/// What a page shows while it lists nothing: what would be here, in a line,
/// and the one button that puts something here.
pub(crate) fn empty(
    icon: AppIcon,
    title: &'static str,
    line: impl Into<SharedString>,
    action: Option<AnyElement>,
    cx: &App,
) -> Div {
    v_flex()
        .items_start()
        .gap_3()
        .py_8()
        .child(
            div()
                .size(px(44.))
                .flex()
                .items_center()
                .justify_center()
                .rounded(cx.theme().radius_lg)
                .border_1()
                .border_color(cx.theme().border)
                .child(Icon::new(icon).size_5().text_color(cx.theme().link)),
        )
        .child(
            div()
                .text_xl()
                .font_family(WORDMARK_FONT)
                .font_weight(FontWeight::MEDIUM)
                .child(title),
        )
        .child(
            div()
                .max_w(px(460.))
                .text_sm()
                .text_color(cx.theme().muted_foreground)
                .child(line.into()),
        )
        .children(action)
}

/// What pressing something does.
pub(crate) type Press = Rc<dyn Fn(&ClickEvent, &mut Window, &mut App)>;

/// A row of a list. Its name and its line are pressed as one, to show or
/// hide what is under it; the button that puts it right and its switch stay
/// their own.
#[derive(IntoElement)]
pub(crate) struct Row {
    id: SharedString,
    mark: Mark,
    name: SharedString,
    tags: Vec<SharedString>,
    line: SharedString,
    line_colour: Option<Hsla>,
    fix: Option<AnyElement>,
    controls: Vec<AnyElement>,
    open: Option<bool>,
    press: Option<Press>,
    under: Option<AnyElement>,
}

impl Row {
    /// A row called `name`, doing as `mark` says. `id` is its own in the
    /// window: its focus and what it remembers are kept under it.
    pub(crate) fn new(
        id: impl Into<SharedString>,
        mark: Mark,
        name: impl Into<SharedString>,
    ) -> Self {
        Self {
            id: id.into(),
            mark,
            name: name.into(),
            tags: Vec::new(),
            line: SharedString::default(),
            line_colour: None,
            fix: None,
            controls: Vec::new(),
            open: None,
            press: None,
            under: None,
        }
    }

    /// What kind of thing it is, beside its name.
    pub(crate) fn tag(mut self, text: impl Into<SharedString>) -> Self {
        self.tags.push(text.into());
        self
    }

    /// How it is doing, in words, under its name; in `colour` when that
    /// says more than muted does.
    pub(crate) fn line(mut self, text: impl Into<SharedString>, colour: Option<Hsla>) -> Self {
        self.line = text.into();
        self.line_colour = colour;
        self
    }

    /// The one button that puts it right, if anything is wrong with it.
    pub(crate) fn fix(mut self, button: Option<impl IntoElement>) -> Self {
        self.fix = button.map(IntoElement::into_any_element);
        self
    }

    /// A control at its right end, after the fix: its switch.
    pub(crate) fn control(mut self, control: impl IntoElement) -> Self {
        self.controls.push(control.into_any_element());
        self
    }

    /// Pressing it shows or hides what is under it; `open` is which it does
    /// now, and turns the caret at its end.
    pub(crate) fn opens(
        mut self,
        open: bool,
        press: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.open = Some(open);
        self.press = Some(Rc::new(press));
        self
    }

    /// What is drawn under it, inside the row's own edges.
    pub(crate) fn under(mut self, under: Option<impl IntoElement>) -> Self {
        self.under = under.map(IntoElement::into_any_element);
        self
    }
}

impl RenderOnce for Row {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let focus = window
            .use_keyed_state(
                SharedString::from(format!("{}-focus", self.id)),
                cx,
                |_, cx| cx.focus_handle(),
            )
            .read(cx)
            .clone();
        let shown = SharedString::from(format!("{}-shown", self.id));
        let theme = cx.theme();
        let (muted, ring, radius, pressed) = (
            theme.muted_foreground,
            theme.ring,
            theme.radius,
            theme.list_active,
        );
        let hover = crate::theme::hover(theme);
        let dim = self.mark == Mark::Off;
        let words = v_flex()
            .flex_1()
            .min_w_0()
            .gap_0p5()
            .child(
                h_flex()
                    .gap_2()
                    .items_center()
                    .child(
                        div()
                            .min_w_0()
                            .truncate()
                            .text_sm()
                            .font_weight(FontWeight::MEDIUM)
                            .when(dim, |this| this.text_color(muted))
                            .child(self.name),
                    )
                    .children(self.tags.into_iter().map(|text| tag(text, cx))),
            )
            .when(!self.line.is_empty(), |this| {
                // Cut to the row at rest, and whole once the row is opened:
                // a reason too long for one line is read there.
                let whole = self.open == Some(true);
                this.child(
                    div()
                        .text_xs()
                        .text_color(self.line_colour.unwrap_or(muted))
                        .when(!whole, |line| line.truncate())
                        .child(self.line),
                )
            });
        let head = div()
            .id(self.id.clone())
            .track_focus(&focus.clone().tab_stop(true));
        let head = super::shown_on_focus(head, shown, &focus, window, cx)
            .flex_1()
            .min_w_0()
            .min_h(ROW_HEIGHT)
            .flex()
            .items_center()
            .gap_3()
            .px_3()
            .py_2()
            // Always there, so the ring the keys bring moves nothing.
            .border_1()
            .border_color(cx.theme().transparent)
            .rounded(radius)
            .focus(|s| s.border_color(ring))
            .child(mark(self.mark, cx))
            .child(words)
            .when_some(self.press.clone(), |this, press| {
                this.cursor_pointer()
                    .hover(|s| s.bg(hover))
                    .active(|s| s.bg(pressed))
                    // A press leaves the keys where they were, as a button's
                    // does; Tab is what puts them on a row.
                    .capture_any_mouse_down(|_, window, _| window.prevent_default())
                    .on_click(move |event, window, cx| press(event, window, cx))
            });
        let caret = self.open.zip(self.press).map(|(open, press)| {
            Button::new(SharedString::from(format!("{}-caret", self.id)))
                .plain(cx)
                .xsmall()
                .icon(Icon::new(if open {
                    IconName::ChevronDown
                } else {
                    IconName::ChevronRight
                }))
                .tooltip(if open { "Show less" } else { "Show more" })
                .on_click(move |event, window, cx| press(event, window, cx))
        });
        v_flex()
            .child(
                h_flex()
                    .items_center()
                    .gap_2()
                    .pr_3()
                    .child(head)
                    .children(self.fix)
                    .children(self.controls)
                    .children(caret),
            )
            .children(
                self.under
                    .map(|under| div().px_3().pb_3().pl(px(38.)).child(under)),
            )
    }
}
