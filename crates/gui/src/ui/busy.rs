//! The marks that say something is under way -- the spinner and the dot
//! beside a reply coming in -- drawn some 25 times a second.
//!
//! gpui's own animations ask for a frame at every refresh of the display, 165
//! a second on a fast panel, and nothing in the window is cached, so one
//! spinner had the whole window -- sidebar, turns, composer -- built again
//! that often for as long as a reply ran. These take their place from the
//! clock instead and ask for the next frame once, a tick from now. A window
//! that is not drawn, minimized or in the tray, asks for nothing more.

use crate::prelude::*;
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
fn next_frame(window: &mut Window, cx: &mut App) {
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

#[cfg(test)]
mod tests {
    // Not `use super::*`: the prelude's gpui `test` macro would shadow the
    // built-in attribute.
    use super::{Asked, TICK, breathing};
    use gpui::{Styled as _, TestAppContext, VisualTestContext, div, point, px, size};

    /// How many windows have a frame asked for.
    fn asked(cx: &mut VisualTestContext) -> usize {
        cx.update(|_, cx| cx.try_global::<Asked>().map_or(0, |asked| asked.0.len()))
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
        assert_eq!(asked(cx), 1);
        // A tick on, the window is drawn again; the reply done, without
        // the dot, nothing more is asked.
        cx.executor().advance_clock(TICK);
        cx.run_until_parked();
        assert_eq!(asked(cx), 0);
        cx.executor().advance_clock(TICK * 10);
        cx.run_until_parked();
        assert_eq!(asked(cx), 0);
    }

    #[gpui::test]
    fn the_dot_stays_still_and_asks_for_nothing_while_motion_is_reduced(cx: &mut TestAppContext) {
        cx.update(|cx| cx.set_reduce_motion(true));
        let cx = cx.add_empty_window();
        assert_eq!(draw_dot(cx), 1.);
        assert_eq!(asked(cx), 0);
    }
}
