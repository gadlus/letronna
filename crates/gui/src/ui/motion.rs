//! How the window moves: three durations, two curves and one distance, so
//! that whatever comes, goes or changes colour moves alike.
//!
//! Motion is there to help the eye follow what changed -- settings opening
//! over the chat, the sidebar or a panel coming in at its edge, a new row in
//! the chat, the colours of a new theme -- and for nothing else: never under
//! the pointer, never holding a key or a click back, and short enough that
//! nobody waits on it. Things come in on a curve that starts quick and
//! settles, and leave faster than they came.
//!
//! All of it reads GPUI's reduce-motion flag (`App::reduce_motion`), which
//! gpui-base sets from the computer's own setting -- Reduce motion on macOS,
//! animation effects on Windows, the desktop portal on Linux -- and which the
//! Motion switch under Settings > Appearance sets too. With it set, GPUI's
//! `with_animation` draws where an animation ends, gpui-component's dialogs and
//! notifications do the same, and `progress` here is 1 at once.

use crate::prelude::*;
use gpui_base::animation::{ease_in_cubic, ease_out_cubic};
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::time::{Duration, Instant};

/// Something leaving, or a small thing changing in place: quick enough
/// that the next click never meets it.
pub(crate) const QUICK: Duration = Duration::from_millis(120);
/// Settings, a panel or the sidebar coming in.
pub(crate) const STANDARD: Duration = Duration::from_millis(180);
/// A new row in the chat, and a new theme's colours: a little longer, as
/// more of the window changes.
pub(crate) const GENTLE: Duration = Duration::from_millis(220);
/// How far, in pixels, something travels as it comes in: enough to say
/// where it came from, too little to read as a move across the window.
pub(crate) const RISE: f32 = 8.;
/// New words of a reply coming in, settling from faint to their full
/// colour: long enough to read as settling, short enough that the newest
/// words are never left faint behind a fast stream.
pub(crate) const SETTLE: Duration = Duration::from_millis(200);
/// How strong a new piece of a reply starts out: about as faint as muted
/// text, not invisible, so the line never looks as if it were missing words.
const FAINT: f32 = 0.45;
/// One pass of the thin sweep under a call that is running: unhurried, as
/// it says only that the call is still at work, not how far along it is.
pub(crate) const PASS: Duration = Duration::from_millis(1400);
/// How small a call's outcome mark starts as it comes in, against its
/// full size.
pub(crate) const MARK_FROM: f32 = 0.85;

/// An animation for something coming in.
pub(crate) fn enter(duration: Duration) -> Animation {
    Animation::new(duration).with_easing(ease_out_cubic)
}

/// An animation for something leaving.
pub(crate) fn exit(duration: Duration) -> Animation {
    Animation::new(duration).with_easing(ease_in_cubic)
}

/// How a reply's text lets in the words the stream adds: each piece starts
/// `FAINT` and settles to its full colour over `SETTLE`. gpui-base's text
/// view repaints it some 30 times a second, not at every refresh, only while
/// a piece is settling, and not at all while motion is reduced.
pub(crate) fn settling() -> gpui_base::text::TextViewMotion {
    let settles = |t: f32| FAINT + (1. - FAINT) * ease_out_cubic(t);
    gpui_base::text::TextViewMotion::default()
        .with_stream_fade(SETTLE)
        .with_stream_fade_easing(gpui_base::motion::Easing::Custom(std::rc::Rc::new(settles)))
}

/// How far in something is that started coming at `since`, eased: 1 once it
/// is there, and at once while motion is reduced. Until then it asks for
/// the next frame.
pub(crate) fn progress(since: Instant, duration: Duration, window: &mut Window, cx: &App) -> f32 {
    if cx.reduce_motion() {
        return 1.;
    }
    let t = fraction(since, cx.background_executor().now(), duration);
    if t < 1. {
        window.request_animation_frame();
    }
    ease_out_cubic(t)
}

/// As `progress`, for something small that comes in beside work under way:
/// the next frame is asked for a tick ahead, as the busy marks ask, rather
/// than at the next refresh.
pub(crate) fn progress_ticked(
    since: Instant,
    duration: Duration,
    window: &mut Window,
    cx: &mut App,
) -> f32 {
    if cx.reduce_motion() {
        return 1.;
    }
    let t = fraction(since, cx.background_executor().now(), duration);
    if t < 1. {
        crate::ui::busy::next_frame(window, cx);
    }
    ease_out_cubic(t)
}

/// The share of `duration` gone by between `since` and `now`, at most 1.
fn fraction(since: Instant, now: Instant, duration: Duration) -> f32 {
    if duration.is_zero() {
        return 1.;
    }
    let gone = now.saturating_duration_since(since).as_secs_f32();
    (gone / duration.as_secs_f32()).min(1.)
}

/// `element`, relatively placed, rising from `RISE` below as it fades in,
/// `t` of the way.
pub(crate) fn rise<E: Styled>(element: E, t: f32) -> E {
    element.top(px(RISE * (1. - t))).opacity(t)
}

/// `element`, relatively placed, coming in from `RISE` out to one side as it
/// fades in, `t` of the way: `side` is -1 for the left edge, 1 for the right.
pub(crate) fn slide<E: Styled>(element: E, t: f32, side: f32) -> E {
    element.left(px(side * RISE * (1. - t))).opacity(t)
}

/// A row still coming in, in a box that moves it; one already in is left
/// as it is. The box has no id, so what is inside keeps its state when it
/// is let go.
pub(crate) fn rising(row: AnyElement, t: f32) -> AnyElement {
    if t >= 1. {
        return row;
    }
    rise(div().relative(), t).child(row).into_any_element()
}

/// Turn motion off while the user's switch says so, and hand the flag back
/// to the computer's setting once it is on again. Cheap to call every frame.
pub(crate) fn follow_setting(on: bool, cx: &mut App) {
    let held = cx.try_global::<Held>().map(|held| held.0);
    match switched(on, held, cx.reduce_motion()) {
        Switch::Keep => {}
        Switch::Hold(system) => {
            cx.set_global(Held(system));
            cx.set_reduce_motion(true);
        }
        Switch::Release(system) => {
            cx.remove_global::<Held>();
            cx.set_reduce_motion(system);
            // gpui-base owns the flag again, as it holds what it last wrote,
            // and reads the system afresh for a change made meanwhile.
            gpui_base::apply_system_reduce_motion(cx);
        }
    }
}

/// What the system asked for when the user's switch turned motion off.
struct Held(bool);

impl Global for Held {}

/// What turning the Motion switch to `on` does to the flag.
#[derive(Debug, PartialEq)]
enum Switch {
    Keep,
    /// Keep the system's wish aside and reduce motion.
    Hold(bool),
    /// Give the system's wish back.
    Release(bool),
}

fn switched(on: bool, held: Option<bool>, flag: bool) -> Switch {
    match (on, held) {
        (false, None) => Switch::Hold(flag),
        (true, Some(system)) => Switch::Release(system),
        _ => Switch::Keep,
    }
}

/// The pieces of the window that come in: the sidebar, the panel beside the
/// chat, the rows of the chat and what the user opens in them.
#[derive(Default)]
pub(crate) struct Moves {
    pub(crate) sidebar: Entrance,
    pub(crate) panel: Entrance,
    pub(crate) rows: Arrivals,
    pub(crate) opened: Opened,
}

/// Something that comes in whenever it is shown, as the sidebar does, and
/// goes at once. Nothing comes in with the window's first frame.
#[derive(Default)]
pub(crate) struct Entrance {
    seen: Cell<bool>,
    shown: Cell<bool>,
    since: Cell<Option<Instant>>,
}

impl Entrance {
    /// Note whether it is shown this frame, and say how far in it is.
    pub(crate) fn track(
        &self,
        shown: bool,
        duration: Duration,
        window: &mut Window,
        cx: &App,
    ) -> f32 {
        self.note(shown, cx.background_executor().now());
        let Some(since) = self.since.get() else {
            return 1.;
        };
        let t = progress(since, duration, window, cx);
        if t >= 1. {
            self.since.set(None);
        }
        t
    }

    fn note(&self, shown: bool, now: Instant) {
        if !shown {
            self.since.set(None);
        } else if !self.shown.get() && self.seen.get() {
            self.since.set(Some(now));
        }
        self.shown.set(shown);
        self.seen.set(true);
    }
}

/// What a row of the chat is, beside its place in the session.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum Row {
    Message,
    /// The files a finished turn changed, at the turn's first message.
    Files,
    /// The approval card, at the place the next message will take.
    Card,
    /// A message waiting for the reply, after the card.
    Queued,
}

/// The rows that came into the session on screen while it was there, and
/// when, so each rises in once: not those it had when it was opened, and
/// not one scrolled back into sight.
#[derive(Default)]
pub(crate) struct Arrivals {
    session: Cell<Option<u64>>,
    reading: Cell<bool>,
    /// Messages before this one were in the session when it came on screen.
    settled: Cell<usize>,
    since: RefCell<HashMap<(Row, usize), Instant>>,
}

impl Arrivals {
    /// Called each frame before the rows are drawn, with the session on
    /// screen, how many messages it has, and whether it is still being read
    /// in: what it holds when it comes on screen, or is read in, or is
    /// rewound, is already there.
    pub(crate) fn note(&self, session: u64, messages: usize, reading: bool) {
        let opened = self.session.get() != Some(session) || reading || self.reading.get();
        if opened || messages < self.settled.get() {
            self.settled.set(messages);
            self.since.borrow_mut().clear();
        }
        self.session.set(Some(session));
        self.reading.set(reading);
    }

    /// `element`, the `row` at `place` in the session, rising in if it came
    /// in a moment ago.
    pub(crate) fn rise(
        &self,
        row: Row,
        place: usize,
        element: AnyElement,
        window: &mut Window,
        cx: &App,
    ) -> AnyElement {
        let Some(since) = self.since(row, place, cx.background_executor().now()) else {
            return element;
        };
        rising(element, progress(since, GENTLE, window, cx))
    }

    /// When the `row` at `place` came in, or None for one that was there
    /// all along.
    pub(crate) fn came(&self, row: Row, place: usize, cx: &App) -> Option<Instant> {
        self.since(row, place, cx.background_executor().now())
    }

    /// When the `row` at `place` was first drawn, or None for one that was
    /// there all along.
    fn since(&self, row: Row, place: usize, now: Instant) -> Option<Instant> {
        if place < self.settled.get() {
            return None;
        }
        Some(*self.since.borrow_mut().entry((row, place)).or_insert(now))
    }
}

/// What can be opened in the chat and folded again.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum Fold {
    /// A reply's thinking.
    Thought,
    /// A call's output.
    Output,
    /// A run of calls drawn as one line.
    Run,
}

/// When each thing in the chat was last opened or folded by the user, by
/// session and place, so what opens eases in. Its height is there at once:
/// gpui lays it out whole, and what a reply's text needs is only known a
/// moment later, once its markdown is laid out.
#[derive(Default)]
pub(crate) struct Opened(HashMap<(Fold, u64, usize), Instant>);

impl Opened {
    /// The user opened or folded `fold` at `key`, a session and a place.
    pub(crate) fn note(&mut self, fold: Fold, key: (u64, usize), cx: &App) {
        let now = cx.background_executor().now();
        self.0.insert((fold, key.0, key.1), now);
    }

    /// `element`, what `fold` at `key` shows open, coming in if it was
    /// opened a moment ago.
    pub(crate) fn ease(
        &self,
        fold: Fold,
        key: (u64, usize),
        element: AnyElement,
        window: &mut Window,
        cx: &mut App,
    ) -> AnyElement {
        let Some(&since) = self.0.get(&(fold, key.0, key.1)) else {
            return element;
        };
        rising(element, progress_ticked(since, STANDARD, window, cx))
    }
}

/// The page colour of the theme left behind, laid over the window as a new
/// one comes in and lifting off it.
struct Veil {
    colour: Hsla,
    since: Instant,
}

impl Global for Veil {}

/// Lay `colour`, the page of the theme being left, over the window.
pub(crate) fn veil(colour: Hsla, cx: &mut App) {
    if cx.reduce_motion() {
        return;
    }
    let since = cx.background_executor().now();
    cx.set_global(Veil { colour, since });
}

/// The veil a change of theme laid, drawn over everything while it lifts.
/// It takes no clicks: a plain fill has no hitbox.
pub(crate) fn veil_layer(window: &mut Window, cx: &mut App) -> Option<Div> {
    let (colour, since) = cx
        .try_global::<Veil>()
        .map(|veil| (veil.colour, veil.since))?;
    let t = progress(since, GENTLE, window, cx);
    if t >= 1. {
        cx.remove_global::<Veil>();
        return None;
    }
    Some(
        div()
            .absolute()
            .top_0()
            .left_0()
            .size_full()
            .bg(colour)
            .opacity(1. - t),
    )
}

#[cfg(test)]
mod tests {
    // Not `use super::*`: the prelude's gpui `test` macro would shadow the
    // built-in attribute.
    use super::{Arrivals, Entrance, Fold, Opened, Row, SETTLE, STANDARD, Switch};
    use super::{fraction, progress_ticked, settling, switched};
    use crate::ui::busy::asked;
    use gpui::{Context, IntoElement, Render, SharedString, TestAppContext, VisualTestContext};
    use gpui::{Window, WindowHandle, div, point, px, size};
    use std::cell::Cell;
    use std::rc::Rc;
    use std::time::{Duration, Instant};

    #[test]
    fn a_moment_is_measured_as_a_share_of_its_duration() {
        let start = Instant::now();
        let later = |ms| start + Duration::from_millis(ms);
        let duration = Duration::from_millis(200);
        assert_eq!(fraction(start, start, duration), 0.);
        assert_eq!(fraction(start, later(50), duration), 0.25);
        assert_eq!(fraction(start, later(400), duration), 1.);
        // A clock read before the start, and a duration of nothing.
        assert_eq!(fraction(later(10), start, duration), 0.);
        assert_eq!(fraction(start, start, Duration::ZERO), 1.);
    }

    #[test]
    fn the_switch_holds_the_systems_wish_and_gives_it_back() {
        // Turned off while the system wants motion, then on again.
        assert_eq!(switched(false, None, false), Switch::Hold(false));
        assert_eq!(switched(true, Some(false), true), Switch::Release(false));
        // Turned off while the system already reduces it.
        assert_eq!(switched(false, None, true), Switch::Hold(true));
        assert_eq!(switched(true, Some(true), true), Switch::Release(true));
        // Left alone either way.
        assert_eq!(switched(true, None, true), Switch::Keep);
        assert_eq!(switched(false, Some(false), true), Switch::Keep);
    }

    #[test]
    fn something_comes_in_when_shown_but_not_with_the_first_frame() {
        let entrance = Entrance::default();
        let now = Instant::now();
        entrance.note(true, now);
        assert_eq!(entrance.since.get(), None);
        entrance.note(false, now);
        entrance.note(true, now);
        assert_eq!(entrance.since.get(), Some(now));
        // Still shown: it keeps coming in from when it was first shown.
        entrance.note(true, now + Duration::from_millis(50));
        assert_eq!(entrance.since.get(), Some(now));
        // Hidden, it goes at once.
        entrance.note(false, now);
        assert_eq!(entrance.since.get(), None);
    }

    #[test]
    fn only_a_row_that_came_in_while_the_session_was_on_screen_rises() {
        let rows = Arrivals::default();
        let now = Instant::now();
        let later = now + Duration::from_secs(5);
        rows.note(1, 3, false);
        assert_eq!(rows.since(Row::Message, 2, now), None);
        assert_eq!(rows.since(Row::Message, 3, now), Some(now));
        // Drawn again, as after scrolling away and back: still from then.
        rows.note(1, 4, false);
        assert_eq!(rows.since(Row::Message, 3, later), Some(now));
        // Another session's rows were all there when it came on screen.
        rows.note(2, 9, false);
        assert_eq!(rows.since(Row::Message, 8, later), None);
        assert_eq!(rows.since(Row::Card, 9, later), Some(later));
    }

    #[test]
    fn rows_read_in_from_disk_or_left_after_a_rewind_do_not_rise() {
        let rows = Arrivals::default();
        let now = Instant::now();
        rows.note(1, 0, true);
        // Read in: everything it holds now was there.
        rows.note(1, 40, false);
        assert_eq!(rows.since(Row::Message, 39, now), None);
        rows.note(1, 40, false);
        assert_eq!(rows.since(Row::Message, 40, now), Some(now));
        // Rewound to 10: the next message, at 10, is new.
        rows.note(1, 10, false);
        assert_eq!(rows.since(Row::Message, 9, now), None);
        assert_eq!(rows.since(Row::Message, 10, now), Some(now));
    }

    /// Draw a frame with what `fold` at `key` shows open, and say how many
    /// windows then have a frame asked for.
    fn draw_open(opened: &Opened, key: (u64, usize), cx: &mut VisualTestContext) -> usize {
        let space = size(px(320.), px(40.));
        cx.draw(point(px(0.), px(0.)), space, |window, cx| {
            opened.ease(Fold::Output, key, div().into_any_element(), window, cx)
        });
        cx.update(|_, cx| asked(cx))
    }

    #[gpui::test]
    fn what_the_user_opens_eases_in_a_tick_at_a_time_and_then_stays(cx: &mut TestAppContext) {
        let cx = cx.add_empty_window();
        let mut opened = Opened::default();
        // Open since the session came up, as a failed call's output is.
        assert_eq!(draw_open(&opened, (1, 4), cx), 0);
        cx.update(|_, cx| opened.note(Fold::Output, (1, 4), cx));
        assert_eq!(draw_open(&opened, (1, 4), cx), 1);
        // In, it asks for nothing more.
        let settled = STANDARD + Duration::from_millis(40);
        cx.executor().advance_clock(settled);
        cx.run_until_parked();
        assert_eq!(draw_open(&opened, (1, 4), cx), 0);
    }

    #[gpui::test]
    fn what_the_user_opens_is_there_at_once_while_motion_is_reduced(cx: &mut TestAppContext) {
        cx.update(|cx| cx.set_reduce_motion(true));
        let cx = cx.add_empty_window();
        let mut opened = Opened::default();
        cx.update(|_, cx| opened.note(Fold::Output, (1, 4), cx));
        assert_eq!(draw_open(&opened, (1, 4), cx), 0);
        let since = cx.update(|_, cx| cx.background_executor().now());
        let t = cx.update(|window, cx| progress_ticked(since, STANDARD, window, cx));
        assert_eq!(t, 1.);
    }

    /// A reply as the chat draws it, counting the frames it is drawn in.
    struct Reply {
        text: SharedString,
        frames: Rc<Cell<usize>>,
    }

    impl Render for Reply {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            self.frames.set(self.frames.get() + 1);
            gpui_base::TextView::markdown("reply", self.text.clone()).motion(settling())
        }
    }

    /// A window drawing the start of a reply, and its frame count.
    fn reply(reduce: bool, cx: &mut TestAppContext) -> (WindowHandle<Reply>, Rc<Cell<usize>>) {
        cx.update(|cx| {
            gpui_component::init(cx);
            cx.set_reduce_motion(reduce);
        });
        let frames = Rc::new(Cell::new(0));
        let shared = frames.clone();
        let window = cx.open_window(size(px(400.), px(200.)), move |_, _| Reply {
            text: "Streaming text".into(),
            frames: shared,
        });
        cx.run_until_parked();
        (window, frames)
    }

    /// The stream adds to the reply, and the window takes it in.
    fn more_words(window: &WindowHandle<Reply>, cx: &mut TestAppContext) {
        let added = window.update(cx, |reply, _, cx| {
            reply.text = "Streaming text arrives a few words at a time".into();
            cx.notify();
        });
        assert!(added.is_ok());
        cx.run_until_parked();
    }

    /// Let `time` go by on the window's clock, and say how many frames it drew.
    fn frames_in(time: Duration, frames: &Rc<Cell<usize>>, cx: &mut TestAppContext) -> usize {
        let before = frames.get();
        cx.executor().advance_clock(time);
        cx.run_until_parked();
        frames.get() - before
    }

    #[gpui::test]
    fn new_words_are_drawn_again_while_they_settle_and_not_after(cx: &mut TestAppContext) {
        let (window, frames) = reply(false, cx);
        more_words(&window, cx);
        // Some 30 frames a second while the new words settle.
        assert_eq!(frames_in(Duration::from_millis(40), &frames, cx), 1);
        assert_eq!(frames_in(Duration::from_millis(40), &frames, cx), 1);
        // Settled, a last frame draws them whole and asks for no more.
        std::thread::sleep(SETTLE + Duration::from_millis(50));
        frames_in(Duration::from_millis(40), &frames, cx);
        assert_eq!(frames_in(Duration::from_secs(1), &frames, cx), 0);
    }

    #[gpui::test]
    fn new_words_come_in_at_once_while_motion_is_reduced(cx: &mut TestAppContext) {
        let (window, frames) = reply(true, cx);
        more_words(&window, cx);
        assert_eq!(frames_in(Duration::from_millis(200), &frames, cx), 0);
    }
}
