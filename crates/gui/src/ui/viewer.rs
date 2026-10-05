//! A picture opened in the window: a made one or a sent one, fitted to the
//! window, zoomed in steps and moved about while zoomed.
//!
//! It is drawn in a dialog, so the chat's keys, the typing that goes to the
//! composer and the text selection in replies stand aside while it is up,
//! as for any dialog, and the focus goes back where it was once it closes.
//! The picture is read through the picture cache no larger than the screen
//! can show it, and at full size only while zoomed past that; both copies
//! are let go of, from memory and the GPU, when the viewer closes.

use crate::prelude::*;
use crate::ui::pictures::Shrunk;
use std::cell::Cell;
use std::rc::Rc;

actions!(
    viewer,
    [
        OpenPicture,
        ZoomIn,
        ZoomOut,
        ZoomFit,
        PanLeft,
        PanRight,
        PanUp,
        PanDown,
        OpenOutside
    ]
);

/// The key context of a picture in the chat that Enter opens.
const PICTURE: &str = "Picture";
/// The key context of the open viewer.
pub(crate) const CONTEXT: &str = "PictureViewer";
/// The viewer's gap from the window's top: under the 34px title bar, with
/// the 12px the bar keeps at its edges, so it clears the traffic lights.
const TOP: f32 = 46.;
/// The viewer's gap from the window's sides and foot.
const MARGIN: f32 = 24.;
/// The height of the bar with the picture's name and the zoom buttons.
const BAR: f32 = 34.;
/// The height of the line of keys under the picture, as the palette's.
const HINTS: f32 = 32.;
/// The zooms past fitting that + and - step through; 1 draws a pixel of
/// the picture on each of the window's.
const STEPS: [f32; 4] = [1., 2., 4., 8.];
/// How far an arrow moves a zoomed picture, as a share of the room.
const PAN_SHARE: f32 = 0.125;
/// How far a wheel turns with Ctrl held before the zoom takes a step, as a
/// browser counts three lines a notch.
const WHEEL_NOTCH: f32 = 100.;
/// The longest side a picture is ever decoded at: 64 MB at most, and as
/// much again on the GPU, while zoomed.
const MAX_SIDE: u32 = 4_096;

/// Open the picture at `path` over the window, or with the system's viewer
/// when it is not one this can read.
pub(crate) fn open(path: PathBuf, window: &mut Window, cx: &mut App) {
    if window.has_active_dialog(cx) {
        return;
    }
    let Ok(size) = image::image_dimensions(&path) else {
        return cx.open_with_system(&path);
    };
    // The screen the window is on, so a larger window later needs no new
    // copy to stay sharp when fitted.
    let screen = window
        .display(cx)
        .map_or(window.viewport_size(), |display| display.bounds().size);
    let screen = f32::from(screen.width).max(f32::from(screen.height)) * window.scale_factor();
    let base_side = (screen.ceil() as u32).min(MAX_SIDE);
    let viewer = cx.new(|cx| Viewer {
        path,
        size,
        base: Shrunk::new(base_side, cx),
        base_side,
        sharp: None,
        zoom: None,
        pan: (0., 0.),
        drag: None,
        wheel: 0.,
        room: (1., 1.),
        fit: 1.,
        bounds: Rc::default(),
        focus: cx.focus_handle(),
    });
    // Only the viewer goes in: the builder runs while the chat draws, where
    // reading the chat would panic.
    let shown = viewer.clone();
    window.open_dialog(cx, move |dialog, window, _| {
        dialog
            .w(px(frame(window).0))
            .margin_top(px(TOP))
            .p_0()
            .close_button(false)
            .child(shown.clone())
    });
    let focus = viewer.read(cx).focus.clone();
    window.focus(&focus, cx);
}

/// `picture`, showing the one at `path`, as a tab stop that Enter or a
/// click opens over the window. `focus` names its focus, kept from frame
/// to frame, and must be unique in the chat.
pub(crate) fn opens(
    picture: Stateful<Div>,
    focus: impl Into<ElementId>,
    path: PathBuf,
    window: &mut Window,
    cx: &mut App,
) -> Stateful<Div> {
    let focus = window
        .use_keyed_state(focus, cx, |_, cx| cx.focus_handle().tab_stop(true))
        .read(cx)
        .clone();
    let ring = focus.is_focused(window).then(|| {
        div()
            .absolute()
            .inset_0()
            .rounded(cx.theme().radius)
            .border_2()
            .border_color(cx.theme().ring)
    });
    let by_key = path.clone();
    picture
        .track_focus(&focus)
        .key_context(PICTURE)
        .cursor_pointer()
        .children(ring)
        // As on a tool row, a press leaves the keys where they were; Tab
        // is what picks a picture out.
        .on_any_mouse_down(|_, window, _| window.prevent_default())
        .on_action(move |_: &OpenPicture, window, cx| open(by_key.clone(), window, cx))
        // gpui also clicks a focused element as Enter comes back up.
        .on_click(move |event, window, cx| {
            if !event.is_keyboard() {
                open(path.clone(), window, cx)
            }
        })
}

/// The viewer's width and height in a window of the size `window` has now.
fn frame(window: &Window) -> (f32, f32) {
    let edges = gpui_component::window_paddings(window);
    let size = window.viewport_size();
    let width = f32::from(size.width - edges.left - edges.right) - 2. * MARGIN;
    let height = f32::from(size.height - edges.top - edges.bottom) - TOP - MARGIN;
    (width.max(240.), height.max(BAR + HINTS + 80.))
}

/// One picture, open over the window.
pub(crate) struct Viewer {
    path: PathBuf,
    /// The picture's own width and height, in pixels.
    size: (u32, u32),
    /// The picture decoded no larger than the screen, which shows it fitted
    /// and while the full one is read.
    base: Entity<Shrunk>,
    /// The longest side `base` is decoded at.
    base_side: u32,
    /// The picture decoded at full size, only while zoomed past `base`.
    sharp: Option<Entity<Shrunk>>,
    /// The zoom picked; `None` fits the picture to the room.
    zoom: Option<f32>,
    /// Where the picture's middle sits from the room's, in pixels.
    pan: (f32, f32),
    /// Where a drag began: the pointer, and the pan then.
    drag: Option<(Point<Pixels>, (f32, f32))>,
    /// How far the wheel has turned with Ctrl held since the last step.
    wheel: f32,
    /// The room the picture is shown in, as last drawn.
    room: (f32, f32),
    /// The zoom that fits the picture to `room`.
    fit: f32,
    /// Where the room was last drawn in the window, for zooming about the
    /// pointer.
    bounds: Rc<Cell<Bounds<Pixels>>>,
    focus: FocusHandle,
}

impl Viewer {
    /// The zoom the picture is drawn at now.
    fn scale(&self) -> f32 {
        self.zoom.unwrap_or(self.fit)
    }

    /// Zoom to `zoom`, keeping the spot of the picture under `at`, a point
    /// from the room's middle, where it is.
    fn zoom_to(&mut self, zoom: Option<f32>, at: (f32, f32), cx: &mut Context<Self>) {
        let from = self.scale();
        self.zoom = zoom;
        self.pan = anchored(self.pan, at, from, self.scale());
        self.keep_in();
        cx.notify();
    }

    /// A zoom step in or out, about the room's middle.
    fn step(&mut self, zoom_in: bool, cx: &mut Context<Self>) {
        let zoom = stepped(self.scale(), self.fit, zoom_in);
        self.zoom_to(zoom, (0., 0.), cx);
    }

    /// Move the picture by `by` pixels.
    fn pan_by(&mut self, by: (f32, f32), cx: &mut Context<Self>) {
        let before = self.pan;
        self.pan = (self.pan.0 + by.0, self.pan.1 + by.1);
        self.keep_in();
        if self.pan != before {
            cx.notify();
        }
    }

    /// Move the picture by a share of the room, `x` and `y` steps across
    /// and down; a right step shows more of the right, so the picture goes
    /// left.
    fn pan_steps(&mut self, x: f32, y: f32, cx: &mut Context<Self>) {
        let by = (-x * self.room.0 * PAN_SHARE, -y * self.room.1 * PAN_SHARE);
        self.pan_by(by, cx);
    }

    /// Keep the pan within what the zoom lets the picture move.
    fn keep_in(&mut self) {
        let scale = self.scale();
        let shown = (self.size.0 as f32 * scale, self.size.1 as f32 * scale);
        self.pan = clamped(self.pan, shown, self.room);
    }

    /// `position` in the window as a point from the room's middle.
    fn off_middle(&self, position: Point<Pixels>) -> (f32, f32) {
        let middle = self.bounds.get().center();
        (
            f32::from(position.x - middle.x),
            f32::from(position.y - middle.y),
        )
    }

    fn open_outside(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        cx.open_with_system(&self.path);
        window.close_dialog(cx);
    }

    fn bar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let muted = theme.muted_foreground;
        let name = self
            .path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        let scale = self.scale();
        let reveal = self.path.clone();
        h_flex()
            .h(px(BAR))
            .flex_none()
            .pl_3()
            .pr_1p5()
            .gap_1()
            .items_center()
            .border_b_1()
            .border_color(theme.border)
            .child(Icon::new(AppIcon::Image).size_4().text_color(muted))
            .child(div().min_w_0().truncate().pl_1().child(name))
            .child(
                div()
                    .flex_none()
                    .pl_1()
                    .text_xs()
                    .text_color(muted)
                    .child(format!("{} × {}", self.size.0, self.size.1)),
            )
            .child(div().flex_1())
            .child(
                Button::new("viewer-out")
                    .plain(cx)
                    .xsmall()
                    .icon(Icon::new(IconName::Minus))
                    .disabled(self.zoom.is_none())
                    .tooltip_with_action("Zoom out", &ZoomOut, Some(CONTEXT))
                    .on_click(cx.listener(|this, _, _, cx| this.step(false, cx))),
            )
            .child(
                div()
                    .flex_none()
                    .w(px(44.))
                    .text_center()
                    .text_xs()
                    .text_color(muted)
                    .child(format!("{:.0}%", scale * 100.)),
            )
            .child(
                Button::new("viewer-in")
                    .plain(cx)
                    .xsmall()
                    .icon(Icon::new(IconName::Plus))
                    .disabled(scale >= STEPS[STEPS.len() - 1])
                    .tooltip_with_action("Zoom in", &ZoomIn, Some(CONTEXT))
                    .on_click(cx.listener(|this, _, _, cx| this.step(true, cx))),
            )
            .child(
                Button::new("viewer-fit")
                    .plain(cx)
                    .xsmall()
                    .label("Fit")
                    .disabled(self.zoom.is_none())
                    .tooltip_with_action("Fit to the window", &ZoomFit, Some(CONTEXT))
                    .on_click(cx.listener(|this, _, _, cx| this.zoom_to(None, (0., 0.), cx))),
            )
            .child(div().w_px().h_4().mx_1().bg(theme.border))
            .child(
                Button::new("viewer-outside")
                    .plain(cx)
                    .xsmall()
                    .icon(Icon::new(AppIcon::PopOut))
                    .tooltip_with_action(
                        "Open with the system's viewer",
                        &OpenOutside,
                        Some(CONTEXT),
                    )
                    .on_click(cx.listener(|this, _, window, cx| this.open_outside(window, cx))),
            )
            .child(
                Button::new("viewer-reveal")
                    .plain(cx)
                    .xsmall()
                    .icon(Icon::new(AppIcon::Folder))
                    .tooltip("Show in folder")
                    .on_click(move |_, _, cx| cx.reveal_path(&reveal)),
            )
            .child(
                Button::new("viewer-close")
                    .plain(cx)
                    .xsmall()
                    .icon(Icon::new(IconName::Close))
                    .tooltip("Close")
                    .on_click(|_, window, cx| window.close_dialog(cx)),
            )
    }

    fn hints(cx: &App) -> impl IntoElement {
        h_flex()
            .h(px(HINTS))
            .flex_none()
            .px_3()
            .gap_3()
            .items_center()
            .border_t_1()
            .border_color(cx.theme().border)
            .text_xs()
            .text_color(cx.theme().muted_foreground)
            .child("+ and − to zoom")
            .child("0 to fit")
            .child("Arrows or a drag to move")
            .child("O to open outside")
            .child("Esc to close")
    }
}

impl Render for Viewer {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let (width, height) = frame(window);
        // Inside the dialog's 1px edge, between the bar and the keys.
        self.room = (width - 2., height - 2. - BAR - HINTS);
        self.fit = fit_scale(self.size, self.room);
        self.keep_in();
        let scale = self.scale();
        let shown = (self.size.0 as f32 * scale, self.size.1 as f32 * scale);
        // Past what the screen-sized copy holds, the full one is read, and
        // it is let go of again once zoomed back.
        let wanted = shown.0.max(shown.1) * window.scale_factor();
        let longest = self.size.0.max(self.size.1);
        if sharper(wanted, longest, self.base_side) {
            if self.sharp.is_none() {
                self.sharp = Some(Shrunk::new(MAX_SIDE, cx));
            }
        } else {
            self.sharp = None;
        }
        // The screen-sized copy stands in until the full one is read, and
        // not under it after: twice drawn, a see-through edge would darken.
        let sharp_ready = self.sharp.as_ref().is_some_and(|s| s.read(cx).ready());
        let at = (
            (self.room.0 - shown.0) / 2. + self.pan.0,
            (self.room.1 - shown.1) / 2. + self.pan.1,
        );
        let path = self.path.clone();
        let muted = cx.theme().muted_foreground;
        let picture = |cache: &Entity<Shrunk>| {
            img(path.clone())
                .image_cache(cache)
                .absolute()
                .left(px(at.0))
                .top(px(at.1))
                .w(px(shown.0))
                .h(px(shown.1))
                .object_fit(ObjectFit::Contain)
        };
        let movable = shown.0 > self.room.0 + 0.5 || shown.1 > self.room.1 + 0.5;
        let bounds = self.bounds.clone();
        let room = div()
            .id("picture-room")
            .relative()
            .flex_none()
            .w(px(self.room.0))
            .h(px(self.room.1))
            .overflow_hidden()
            .bg(cx.theme().secondary)
            .when(movable, |room| {
                room.cursor(if self.drag.is_some() {
                    CursorStyle::ClosedHand
                } else {
                    CursorStyle::OpenHand
                })
            })
            .child(
                canvas(move |placed, _, _| bounds.set(placed), |_, _, _, _| {})
                    .absolute()
                    .size_full(),
            )
            .when(!sharp_ready, |room| {
                room.child(picture(&self.base).with_fallback(move || {
                    div()
                        .size_full()
                        .flex()
                        .items_center()
                        .justify_center()
                        .text_color(muted)
                        .child("This picture cannot be shown here; O opens it outside.")
                        .into_any_element()
                }))
            })
            .children(self.sharp.as_ref().map(&picture))
            .on_scroll_wheel(cx.listener(|this, event: &ScrollWheelEvent, window, cx| {
                let delta = event.delta.pixel_delta(window.line_height());
                if event.modifiers.secondary() {
                    // A step a notch, however finely a touchpad turns it, as
                    // the browser zooms.
                    this.wheel += f32::from(delta.y);
                    if this.wheel.abs() >= WHEEL_NOTCH {
                        let zoom_in = this.wheel > 0.;
                        this.wheel = 0.;
                        let at = this.off_middle(event.position);
                        let zoom = stepped(this.scale(), this.fit, zoom_in);
                        this.zoom_to(zoom, at, cx);
                    }
                } else {
                    this.pan_by((f32::from(delta.x), f32::from(delta.y)), cx);
                }
                cx.stop_propagation();
            }))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, event: &MouseDownEvent, _, cx| {
                    // A double click zooms in about the pointer, and out
                    // to fit again.
                    if event.click_count == 2 {
                        let zoom = match this.zoom {
                            None => stepped(this.scale(), this.fit, true),
                            Some(_) => None,
                        };
                        let at = this.off_middle(event.position);
                        this.zoom_to(zoom, at, cx);
                    }
                    this.drag = Some((event.position, this.pan));
                    cx.notify();
                }),
            )
            .on_mouse_move(cx.listener(|this, event: &MouseMoveEvent, _, cx| {
                let Some((start, from)) = this.drag else {
                    return;
                };
                if event.pressed_button != Some(MouseButton::Left) {
                    this.drag = None;
                    return cx.notify();
                }
                let moved = event.position - start;
                this.pan = (from.0 + f32::from(moved.x), from.1 + f32::from(moved.y));
                this.keep_in();
                cx.notify();
            }))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| {
                    this.drag = None;
                    cx.notify();
                }),
            );
        v_flex()
            .id("picture-viewer")
            .key_context(CONTEXT)
            .track_focus(&self.focus)
            .w(px(width - 2.))
            .h(px(height - 2.))
            .overflow_hidden()
            .on_action(cx.listener(|this, _: &ZoomIn, _, cx| this.step(true, cx)))
            .on_action(cx.listener(|this, _: &ZoomOut, _, cx| this.step(false, cx)))
            .on_action(cx.listener(|this, _: &ZoomFit, _, cx| this.zoom_to(None, (0., 0.), cx)))
            .on_action(cx.listener(|this, _: &PanLeft, _, cx| this.pan_steps(-1., 0., cx)))
            .on_action(cx.listener(|this, _: &PanRight, _, cx| this.pan_steps(1., 0., cx)))
            .on_action(cx.listener(|this, _: &PanUp, _, cx| this.pan_steps(0., -1., cx)))
            .on_action(cx.listener(|this, _: &PanDown, _, cx| this.pan_steps(0., 1., cx)))
            .on_action(
                cx.listener(|this, _: &OpenOutside, window, cx| this.open_outside(window, cx)),
            )
            .child(self.bar(cx))
            .child(room)
            .child(Self::hints(cx))
    }
}

/// The zoom that fits a picture of `size` in `room`, never past a pixel of
/// it on each of the window's.
fn fit_scale(size: (u32, u32), room: (f32, f32)) -> f32 {
    let (width, height) = (size.0.max(1) as f32, size.1.max(1) as f32);
    (room.0 / width).min(room.1 / height).clamp(0.001, 1.)
}

/// Whether a picture `longest` pixels a side, drawn `wanted` pixels a side,
/// needs a copy sharper than the screen-sized one `base_side` a side: not
/// when that holds all of it, nor when it is already as large as any copy.
fn sharper(wanted: f32, longest: u32, base_side: u32) -> bool {
    wanted > base_side as f32 && longest > base_side && base_side < MAX_SIDE
}

/// The zoom a step from `scale` in or out goes to: the next of `STEPS`,
/// and from the first one out, fitted (`None`) again.
fn stepped(scale: f32, fit: f32, zoom_in: bool) -> Option<f32> {
    let last = STEPS[STEPS.len() - 1];
    if zoom_in {
        return Some(
            STEPS
                .into_iter()
                .find(|&step| step > scale * 1.001)
                .unwrap_or(last),
        );
    }
    STEPS
        .into_iter()
        .rev()
        .find(|&step| step < scale * 0.999)
        .filter(|&step| step > fit * 1.001)
}

/// The pan that keeps the spot of the picture under `at` there when its
/// zoom goes from `from` to `to`; both points are from the room's middle.
fn anchored(pan: (f32, f32), at: (f32, f32), from: f32, to: f32) -> (f32, f32) {
    let grown = to / from;
    (at.0 - (at.0 - pan.0) * grown, at.1 - (at.1 - pan.1) * grown)
}

/// `pan` kept so a picture `shown` larger than `room` never shows its edge
/// inside it, and one smaller stays in the middle.
fn clamped(pan: (f32, f32), shown: (f32, f32), room: (f32, f32)) -> (f32, f32) {
    let limit = |shown: f32, room: f32| ((shown - room) / 2.).max(0.);
    let (x, y) = (limit(shown.0, room.0), limit(shown.1, room.1));
    (pan.0.clamp(-x, x), pan.1.clamp(-y, y))
}

#[cfg(test)]
mod tests {
    // Not `use super::*`: the prelude re-exports gpui, whose `test` macro
    // would shadow the built-in attribute.
    use super::{MAX_SIDE, anchored, clamped, fit_scale, sharper, stepped};

    #[test]
    fn a_big_picture_fits_the_room_and_a_small_one_is_not_blown_up() {
        assert_eq!(fit_scale((2_000, 1_000), (1_000., 800.)), 0.5);
        assert_eq!(fit_scale((1_000, 2_000), (1_000., 800.)), 0.4);
        assert_eq!(fit_scale((64, 64), (1_000., 800.)), 1.);
    }

    #[test]
    fn zoom_steps_go_from_fitted_to_eight_times_and_back() {
        assert_eq!(stepped(0.4, 0.4, true), Some(1.));
        assert_eq!(stepped(1., 0.4, true), Some(2.));
        assert_eq!(stepped(8., 0.4, true), Some(8.));
        assert_eq!(stepped(2., 0.4, false), Some(1.));
        assert_eq!(stepped(1., 0.4, false), None);
        // A picture that fits at a pixel for a pixel has no 100% step of
        // its own: out from 200% it is fitted again.
        assert_eq!(stepped(1., 1., true), Some(2.));
        assert_eq!(stepped(2., 1., false), None);
        assert_eq!(stepped(0.4, 0.4, false), None);
    }

    #[test]
    fn zooming_keeps_the_spot_under_the_pointer() {
        // Twice the size about a point 100px right of the middle: what was
        // 100px right of the picture's middle is now 200px right of it.
        assert_eq!(anchored((0., 0.), (100., 0.), 1., 2.), (-100., 0.));
        // About the middle, the pan grows with the picture.
        assert_eq!(anchored((30., -10.), (0., 0.), 1., 2.), (60., -20.));
    }

    #[test]
    fn a_zoomed_picture_never_shows_its_edge_and_a_fitted_one_stays_centred() {
        let room = (800., 600.);
        assert_eq!(
            clamped((500., -500.), (1_600., 1_200.), room),
            (400., -300.)
        );
        assert_eq!(clamped((50., 20.), (400., 300.), room), (0., 0.));
        // Wider than the room but not taller: it moves across only.
        assert_eq!(clamped((-90., 90.), (1_000., 300.), room), (-90., 0.));
    }

    #[test]
    fn a_screen_sized_copy_at_the_largest_size_is_never_read_twice() {
        // A 6000px photo zoomed past a 4K screen's copy is read again larger.
        assert!(sharper(6_000., 6_000, 3_840));
        // On a 5K screen the screen's copy is already the largest there is.
        assert!(!sharper(6_000., 6_000, MAX_SIDE));
        // Nor past a picture the screen's copy holds whole.
        assert!(!sharper(3_000., 2_000, 2_560));
        assert!(!sharper(2_000., 6_000, 2_560));
    }
}
