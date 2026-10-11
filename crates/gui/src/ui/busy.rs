//! The marks that say something is under way -- the spinner, the dot
//! beside a reply coming in, the light passing across a label such as
//! "Thinking…", and the sweep under a running call -- drawn some 25 times
//! a second.
//!
//! gpui's own animations ask for a frame at every refresh of the display, 165
//! a second on a fast panel, and nothing in the window is cached, so one
//! spinner had the whole window -- sidebar, turns, composer -- built again
//! that often for as long as a reply ran. These take their place from the
//! clock instead and ask for the next frame once, a tick from now. A window
//! that is not drawn, minimized or in the tray, asks for nothing more.

use crate::prelude::*;
use gpui_component::Colorize as _;
use std::collections::HashSet;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

/// From one frame of a busy mark to asking for the next: with the frame's
/// own time, some 25 frames a second, smooth for marks this small.
const TICK: Duration = Duration::from_millis(33);
/// One turn of the spinner, as long as gpui-component's takes.
const TURN: Duration = Duration::from_millis(800);
/// One breath of the dot beside a reply coming in.
const BREATH: Duration = Duration::from_millis(1000);

/// gpui-component's spinner, turning on the clock instead of on every frame.
#[derive(IntoElement)]
pub(crate) struct Spinner {
    size: gpui_component::Size,
    color: Option<Hsla>,
}

impl Spinner {
    pub(crate) fn new() -> Self {
        Self {
            size: gpui_component::Size::Medium,
            color: None,
        }
    }

    pub(crate) fn color(mut self, color: Hsla) -> Self {
        self.color = Some(color);
        self
    }
}

impl Sizable for Spinner {
    fn with_size(mut self, size: impl Into<gpui_component::Size>) -> Self {
        self.size = size.into();
        self
    }
}

impl RenderOnce for Spinner {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        // Still, as gpui-component's is, while motion is reduced.
        let turned = if cx.reduce_motion() {
            0.
        } else {
            next_frame(window, cx);
            ease_in_out(phase(TURN))
        };
        div().child(
            Icon::new(IconName::Loader)
                .with_size(self.size)
                .when_some(self.color, |icon, color| icon.text_color(color))
                .transform(Transformation::rotate(percentage(turned))),
        )
    }
}

/// A label for what is under way -- thinking, a call being written or run
/// -- with a soft light passing across it once every `motion::SWEEP`, each
/// letter lit from the colour it is drawn in towards the theme's
/// foreground as the light goes by. Still while motion is reduced. In place
/// of gpui-component's `ShimmerText`, which asks for every refresh.
#[derive(IntoElement)]
pub(crate) struct Shimmer {
    text: SharedString,
    /// Where the text starts among the letters of the whole label, and how
    /// many letters the light crosses, for a label drawn in parts.
    at: usize,
    of: Option<usize>,
}

impl Shimmer {
    pub(crate) fn new(text: impl Into<SharedString>) -> Self {
        Self {
            text: text.into(),
            at: 0,
            of: None,
        }
    }

    /// A running call's name and what it was asked, drawn apart, under one
    /// light. A long summary is cut short where the row cuts it, so the
    /// light comes round again where it can be seen.
    pub(crate) fn label(name: impl Into<SharedString>, summary: String) -> (Self, Self) {
        let name = Self::new(name);
        let at = name.text.chars().count() + 1;
        let of = Some(at + summary.chars().count().min(SEEN));
        let summary = Self::new(summary);
        (Self { of, ..name }, Self { at, of, ..summary })
    }
}

/// About as many letters of a running call's summary as a row shows.
const SEEN: usize = 48;

impl RenderOnce for Shimmer {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        if cx.reduce_motion() {
            return StyledText::new(self.text);
        }
        next_frame(window, cx);
        // From the colour the label is drawn in, which its row sets.
        let (dim, lit) = (window.text_style().color, cx.theme().foreground);
        let of = self.of.unwrap_or_else(|| self.text.chars().count());
        let t = phase(crate::ui::motion::SWEEP);
        let tints = tints(&self.text, self.at, of, t, (dim, lit));
        StyledText::new(self.text).with_highlights(tints)
    }
}

/// How many letters either side of its middle the light reaches.
const REACH: f32 = 5.;
/// How much lighter or darker an odd letter is than an even one lit alike:
/// too little to see, enough that two neighbours are never quite the same
/// colour. Not its opacity, which gpui blends away over the colour beneath.
const TWIN: f32 = 0.002;

/// The colour of each letter of `text`, `t` of the way through a pass of
/// the light across `of` letters, `text` starting at letter `at`: from
/// `dim` to `lit`. Every letter gets one, and neighbours never quite the
/// same one, so the text is cut into the same runs in every frame -- gpui
/// shapes a line again when its runs change, and otherwise reuses the line
/// it shaped the frame before -- and only the colours change.
fn tints(
    text: &str,
    at: usize,
    of: usize,
    t: f32,
    (dim, lit): (Hsla, Hsla),
) -> Vec<(std::ops::Range<usize>, HighlightStyle)> {
    let tint = |(n, (start, letter)): (usize, (usize, char))| {
        let mut color = lit.mix_oklab(dim, light(at + n, of, t));
        if n % 2 == 1 {
            color.l += if color.l > 0.5 { -TWIN } else { TWIN };
        }
        (start..start + letter.len_utf8(), color.into())
    };
    text.char_indices().enumerate().map(tint).collect()
}

/// How lit letter `n` of `of` is, from 0 to 1, with the light `t` of the
/// way across: it starts and ends out of sight on either side, so each
/// pass comes in from the left.
fn light(n: usize, of: usize, t: f32) -> f32 {
    let middle = t * (of as f32 + 2. * REACH) - REACH;
    let near = 1. - ((n as f32 + 0.5 - middle).abs() / REACH).min(1.);
    near * near * (3. - 2. * near)
}

/// A thin line under a call that is running, a short soft stretch of the
/// theme's accent passing along it once every `motion::PASS`: it says the
/// call is at work, not how far along. An empty line of the same height
/// while motion is reduced, so the row does not move.
#[derive(IntoElement)]
pub(crate) struct Sweep;

/// How much of the line the moving stretch covers.
const STRETCH: f32 = 0.3;

impl RenderOnce for Sweep {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let line = div().relative().w_full().h(px(1.5)).overflow_hidden();
        if cx.reduce_motion() {
            return line;
        }
        next_frame(window, cx);
        let accent = cx.theme().primary.opacity(0.6);
        let clear = accent.opacity(0.);
        // From just out of sight at the left to just out of sight at the right.
        let at = ease_in_out(phase(crate::ui::motion::PASS)) * (1. + STRETCH) - STRETCH;
        let half = |from, to| {
            let (from, to) = (linear_color_stop(from, 0.), linear_color_stop(to, 1.));
            div().h_full().flex_1().bg(linear_gradient(90., from, to))
        };
        line.child(
            h_flex()
                .absolute()
                .top_0()
                .h_full()
                .left(relative(at))
                .w(relative(STRETCH))
                .child(half(clear, accent))
                .child(half(accent, clear)),
        )
    }
}

/// The opacity of the dot beside a reply coming in, now: in and out once a
/// second.
pub(crate) fn breathing(window: &mut Window, cx: &mut App) -> f32 {
    if cx.reduce_motion() {
        return 1.;
    }
    next_frame(window, cx);
    let wave = 1.0 - (ease_in_out(phase(BREATH)) * 2.0 - 1.0).abs();
    0.25 + 0.75 * wave
}

/// How far through a `period` the clock is, from 0 to 1. Whole milliseconds,
/// which an f32 of seconds since start would lose after a few days.
fn phase(period: Duration) -> f32 {
    static START: OnceLock<Instant> = OnceLock::new();
    let elapsed = START.get_or_init(Instant::now).elapsed().as_millis();
    let period = period.as_millis().max(1);
    (elapsed % period) as f32 / period as f32
}

/// The windows a frame is asked for already.
#[derive(Default)]
struct Asked(HashSet<WindowId>);

impl Global for Asked {}

/// Ask for this window to be drawn again a tick from now, once however many
/// marks it draws.
pub(crate) fn next_frame(window: &mut Window, cx: &mut App) {
    let handle = window.window_handle();
    let id = handle.window_id();
    if !cx.default_global::<Asked>().0.insert(id) {
        return;
    }
    cx.spawn(async move |cx| {
        cx.background_executor().timer(TICK).await;
        cx.update(|cx| {
            cx.default_global::<Asked>().0.remove(&id);
            let _ = handle.update(cx, |_, window, _| window.refresh());
        });
    })
    .detach();
}

/// How many windows have a frame asked for, for the window's tests.
#[cfg(test)]
pub(crate) fn asked(cx: &App) -> usize {
    cx.try_global::<Asked>().map_or(0, |asked| asked.0.len())
}

#[cfg(test)]
mod tests {
    // Not `use super::*`: the prelude's gpui `test` macro would shadow the
    // built-in attribute.
    use super::{Shimmer, Sweep, TICK, asked, breathing, light, tints};
    use gpui::prelude::FluentBuilder as _;
    use gpui::{ParentElement as _, Styled as _, TestAppContext, VisualTestContext, div};
    use gpui::{point, px, size};

    /// Draw a frame with a running call's sweep in it, or without one.
    fn draw(cx: &mut VisualTestContext, running: bool) {
        let space = size(px(320.), px(20.));
        cx.draw(point(px(0.), px(0.)), space, |_, _| {
            div().w(px(320.)).when(running, |line| line.child(Sweep))
        });
    }

    fn asked_now(cx: &mut VisualTestContext) -> usize {
        cx.update(|_, cx| asked(cx))
    }

    #[gpui::test]
    fn a_running_call_asks_for_one_frame_a_tick_ahead_and_none_once_it_is_done(
        cx: &mut TestAppContext,
    ) {
        cx.update(gpui_component::theme::init);
        let cx = cx.add_empty_window();
        draw(cx, true);
        assert_eq!(asked_now(cx), 1);
        cx.executor().advance_clock(TICK);
        cx.run_until_parked();
        assert_eq!(asked_now(cx), 0);
        // Done: drawn without its sweep, the window asks for nothing more.
        draw(cx, false);
        cx.executor().advance_clock(TICK * 10);
        cx.run_until_parked();
        assert_eq!(asked_now(cx), 0);
    }

    #[gpui::test]
    fn a_running_call_asks_for_nothing_while_motion_is_reduced(cx: &mut TestAppContext) {
        cx.update(|cx| {
            gpui_component::theme::init(cx);
            cx.set_reduce_motion(true);
        });
        let cx = cx.add_empty_window();
        draw(cx, true);
        assert_eq!(asked_now(cx), 0);
    }

    /// Draw a frame with the dot at the end of a reply coming in, and say
    /// how strong it was drawn.
    fn draw_dot(cx: &mut VisualTestContext) -> f32 {
        let mut drawn = 0.;
        let space = size(px(40.), px(20.));
        cx.draw(point(px(0.), px(0.)), space, |window, cx| {
            drawn = breathing(window, cx);
            div().size(px(6.)).opacity(drawn)
        });
        drawn
    }

    #[gpui::test]
    fn the_dot_asks_for_one_frame_a_tick_ahead_and_none_once_it_is_gone(cx: &mut TestAppContext) {
        let cx = cx.add_empty_window();
        draw_dot(cx);
        draw_dot(cx);
        assert_eq!(asked_now(cx), 1);
        // A tick on, the window is drawn again; the reply done, without
        // the dot, nothing more is asked.
        cx.executor().advance_clock(TICK);
        cx.run_until_parked();
        assert_eq!(asked_now(cx), 0);
        cx.executor().advance_clock(TICK * 10);
        cx.run_until_parked();
        assert_eq!(asked_now(cx), 0);
    }

    #[gpui::test]
    fn the_dot_stays_still_and_asks_for_nothing_while_motion_is_reduced(cx: &mut TestAppContext) {
        cx.update(|cx| cx.set_reduce_motion(true));
        let cx = cx.add_empty_window();
        assert_eq!(draw_dot(cx), 1.);
        assert_eq!(asked_now(cx), 0);
    }

    /// Draw a frame with `labels` in it.
    fn draw_labels(cx: &mut VisualTestContext, labels: &'static [&'static str]) {
        let space = size(px(320.), px(80.));
        cx.draw(point(px(0.), px(0.)), space, |_, _| {
            div().children(labels.iter().map(|label| Shimmer::new(*label)))
        });
    }

    #[gpui::test]
    fn a_label_at_work_asks_for_one_frame_a_tick_ahead_and_none_once_it_is_gone(
        cx: &mut TestAppContext,
    ) {
        cx.update(gpui_component::theme::init);
        let cx = cx.add_empty_window();
        draw_labels(cx, &["Thinking…", "run_command"]);
        assert_eq!(asked_now(cx), 1);
        // A tick on, the window is drawn again; with nothing under way, it
        // asks for nothing more.
        cx.executor().advance_clock(TICK);
        cx.run_until_parked();
        assert_eq!(asked_now(cx), 0);
        draw_labels(cx, &[]);
        assert_eq!(asked_now(cx), 0);
        cx.executor().advance_clock(TICK * 10);
        cx.run_until_parked();
        assert_eq!(asked_now(cx), 0);
    }

    #[gpui::test]
    fn a_label_asks_for_nothing_while_motion_is_reduced(cx: &mut TestAppContext) {
        cx.update(|cx| {
            gpui_component::theme::init(cx);
            cx.set_reduce_motion(true);
        });
        let cx = cx.add_empty_window();
        draw_labels(cx, &["Thinking…"]);
        assert_eq!(asked_now(cx), 0);
    }

    #[test]
    fn the_light_crosses_a_label_from_left_to_right() {
        // Out of sight before the pass starts and after it ends.
        assert!((0..9).all(|n| light(n, 9, 0.) == 0. && light(n, 9, 1.) == 0.));
        let brightest = |t| (0..9).max_by(|a, b| light(*a, 9, t).total_cmp(&light(*b, 9, t)));
        assert!(brightest(0.3) < brightest(0.5) && brightest(0.5) < brightest(0.7));
        assert!((0..9).all(|n| (0. ..=1.).contains(&light(n, 9, 0.6))));
    }

    #[test]
    fn every_letter_is_its_own_run_in_every_frame() {
        let label = "Thinking… npm test";
        let (dim, lit) = (gpui::hsla(0.6, 0.2, 0.6, 1.), gpui::hsla(0.6, 0.2, 0.9, 1.));
        for t in [0., 0.25, 0.5, 0.9, 1.] {
            let tints = tints(label, 0, 18, t, (dim, lit));
            let ranges: Vec<_> = tints.iter().map(|(range, _)| range.clone()).collect();
            let letters: Vec<_> = label
                .char_indices()
                .map(|(at, c)| at..at + c.len_utf8())
                .collect();
            assert_eq!(ranges, letters);
            // As the text's own colour takes them.
            let drawn: Vec<_> = tints
                .iter()
                .map(|(_, tint)| tint.color.map(|c| dim.blend(c)))
                .collect();
            let apart = drawn.windows(2).all(|two| two[0] != two[1]);
            assert!(apart, "two neighbours alike at {t}");
        }
    }

    #[test]
    fn the_parts_of_a_label_share_one_light() {
        let (name, summary) = Shimmer::label("run_command", "npm test".to_owned());
        assert_eq!((name.at, name.of), (0, Some(20)));
        assert_eq!((summary.at, summary.of), (12, Some(20)));
        // While it is on the name, the summary is dark.
        assert!((0..11).any(|n| light(n, 20, 0.3) > 0.));
        assert!((12..20).all(|n| light(n, 20, 0.2) == 0.));
        // A long summary is crossed only as far as a row shows it.
        let (_, long) = Shimmer::label("run_command", "x".repeat(300));
        assert_eq!(long.of, Some(12 + super::SEEN));
    }
}
