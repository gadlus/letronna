//! The browser beside the chat: the page the agent is using, live, with a
//! pointer that goes where the agent is about to act before it acts.
//!
//! Letronna has no web view of its own. These are the browser's pictures of
//! the page, which it sends only when the page changes -- at most twelve a
//! second, and none once the panel is closed -- drawn as images. A wide
//! window docks the panel beside the chat; a narrow one floats it in the
//! corner, where it covers the least.

use crate::prelude::*;
use letronna_core::watch::{self, Sight, Watching};
use std::sync::Arc;
use std::time::Duration;

/// A window at least this wide docks the panel beside the chat -- Letronna's
/// own window opens this wide -- and a narrower one floats it in a corner.
const DOCK_WIDTH: Pixels = px(1100.);
/// How much of the window the docked panel takes, and its bounds: less, and
/// a page's words are too small to follow; more, and the chat's composer is
/// squeezed.
const DOCK_SHARE: f32 = 0.34;
const DOCK_MIN: Pixels = px(340.);
const DOCK_MAX: Pixels = px(640.);
/// How wide the floating panel is.
const FLOAT_WIDTH: Pixels = px(320.);
/// How long a pointer's move and its click take together.
const POINTER_ANIMATION: Duration = Duration::from_millis(900);
/// The pointer's dot, and the ring its click leaves at its widest.
const DOT: f32 = 14.;
const RING: f32 = 46.;

pub(crate) struct BrowserView {
    /// Read the browser's pictures, and everything else it shows, while
    /// the panel is open. Dropping them stops the watching.
    watching: Option<[Task<()>; 2]>,
    /// Closed by the user during this turn, so it stays closed until the
    /// next one.
    pub(crate) dismissed: bool,
    frame: Option<Arc<RenderImage>>,
    /// The page's size in CSS pixels, which pointers are given in.
    size: (f64, f64),
    title: String,
    url: String,
    pointer: Option<Pointer>,
    /// Pointers so far, so each gets an animation of its own.
    pointers: usize,
}

/// Where the agent is about to act, in the page's CSS pixels.
struct Pointer {
    from: (f64, f64),
    to: (f64, f64),
    /// The box of what is acted on, until the page changes.
    area: Option<[f64; 4]>,
    doing: String,
}

impl BrowserView {
    pub(crate) fn new() -> Self {
        Self {
            watching: None,
            dismissed: false,
            frame: None,
            size: (0., 0.),
            title: String::new(),
            url: String::new(),
            pointer: None,
            pointers: 0,
        }
    }

    /// Open the panel, unless it was closed this turn, and start watching.
    pub(crate) fn open(&mut self, cx: &mut Context<Self>) {
        if self.watching.is_some() || self.dismissed {
            return;
        }
        let Watching { frames, sights } = watch::watch();
        let pictures = cx.spawn(async move |this, cx| {
            while let Ok(frame) = frames.recv().await {
                let (width, height) = (frame.width, frame.height);
                let decode = cx
                    .background_executor()
                    .spawn(async move { picture(&frame.jpeg) });
                let Some(image) = decode.await else {
                    continue;
                };
                let shown = this.update(cx, |view, cx| {
                    view.show(Some(Arc::new(image)), cx);
                    view.size = (width, height);
                    cx.notify();
                });
                if shown.is_err() {
                    break;
                }
            }
        });
        let rest = cx.spawn(async move |this, cx| {
            while let Ok(sight) = sights.recv().await {
                let shown = this.update(cx, |view, cx| {
                    match sight {
                        Sight::Pointer { x, y, area, doing } => view.point((x, y), area, doing),
                        Sight::Page { title, url } => {
                            // The box drawn was on the page before.
                            if view.url != url
                                && let Some(pointer) = view.pointer.as_mut()
                            {
                                pointer.area = None;
                            }
                            view.title = title;
                            view.url = url;
                        }
                        Sight::Closed => {
                            view.show(None, cx);
                            view.pointer = None;
                            view.title.clear();
                            view.url.clear();
                        }
                    }
                    cx.notify();
                });
                if shown.is_err() {
                    break;
                }
            }
        });
        self.watching = Some([pictures, rest]);
        cx.notify();
    }

    /// Whether the panel is open and docked beside the chat, which then
    /// needs its full width.
    pub(crate) fn docked(&self, window: &Window) -> bool {
        self.watching.is_some() && window.viewport_size().width >= DOCK_WIDTH
    }

    fn close(&mut self, cx: &mut Context<Self>) {
        self.watching = None;
        self.dismissed = true;
        self.show(None, cx);
        cx.notify();
    }

    /// Put up a new picture, and give the one before back to the GPU: gpui
    /// keeps every picture it has drawn until told it is done with it, and a
    /// page that moves would fill the atlas with them.
    fn show(&mut self, frame: Option<Arc<RenderImage>>, cx: &mut Context<Self>) {
        if let Some(old) = std::mem::replace(&mut self.frame, frame) {
            cx.drop_image(old, None);
        }
    }

    fn point(&mut self, to: (f64, f64), area: [f64; 4], doing: String) {
        let from = match &self.pointer {
            Some(pointer) => pointer.to,
            None => (self.size.0 / 2., self.size.1 / 2.),
        };
        self.pointer = Some(Pointer {
            from,
            to,
            area: Some(area),
            doing,
        });
        self.pointers += 1;
    }

    fn header(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let title = match self.title.trim() {
            "" => "Browser".to_owned(),
            title => title.to_owned(),
        };
        h_flex()
            .h(px(34.))
            .flex_shrink_0()
            .px_3()
            .gap_2()
            .items_center()
            .border_b_1()
            .border_color(cx.theme().border)
            .child(
                // Lit while there is a page to show.
                div()
                    .size(px(6.))
                    .flex_shrink_0()
                    .rounded_full()
                    .bg(match self.frame {
                        Some(_) => cx.theme().success,
                        None => cx.theme().muted_foreground,
                    }),
            )
            .child(
                div()
                    .min_w_0()
                    .flex_shrink()
                    .truncate()
                    .text_xs()
                    .font_family(HEADING_FONT)
                    .font_weight(FontWeight::MEDIUM)
                    .child(title),
            )
            .child(
                div()
                    .min_w_0()
                    .flex_1()
                    .truncate()
                    .text_xs()
                    .font_family(MONO_FONT)
                    .text_color(cx.theme().muted_foreground)
                    .child(self.url.clone()),
            )
            .child(
                Button::new("hide-browser")
                    .ghost()
                    .xsmall()
                    .icon(Icon::new(IconName::Close))
                    .tooltip("Hide the browser")
                    .on_click(cx.listener(|view, _, _, cx| view.close(cx))),
            )
    }

    /// The page, with the pointer over it.
    fn screen(&self, cx: &mut Context<Self>) -> AnyElement {
        let (width, height) = self.size;
        let Some(frame) = self.frame.clone().filter(|_| width >= 1. && height >= 1.) else {
            return div()
                .h(px(160.))
                .flex()
                .items_center()
                .justify_center()
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .child("The page shows here while the agent uses the browser.")
                .into_any_element();
        };
        // The page's own shape, so the picture fills the box exactly and a
        // pointer placed over the box lands where it should on the page.
        let mut screen = div().relative().w_full().flex_shrink_0().overflow_hidden();
        screen.style().aspect_ratio = Some((width / height) as f32);
        screen
            .child(img(frame).size_full())
            .children(
                self.pointer
                    .as_ref()
                    .map(|pointer| self.pointer_over(pointer, cx)),
            )
            .into_any_element()
    }

    /// The pointer, the ring its click leaves, and the box of what it acts
    /// on, as one animation: the dot eases in to land as the action happens,
    /// the ring spreads from there, and the box stays, faint, till the page
    /// changes.
    fn pointer_over(&self, pointer: &Pointer, cx: &mut Context<Self>) -> impl IntoElement {
        let (width, height) = self.size;
        let at = |(x, y): (f64, f64)| ((x / width) as f32, (y / height) as f32);
        let (from, to) = (at(pointer.from), at(pointer.to));
        let colour = cx.theme().primary;
        let id = self.pointers;
        let area = pointer.area.map(|[x, y, w, h]| {
            div()
                .absolute()
                .left(relative((x / width) as f32))
                .top(relative((y / height) as f32))
                .w(relative((w / width) as f32))
                .h(relative((h / height) as f32))
                .rounded(px(4.))
                .border_2()
                .border_color(colour)
                .bg(colour.opacity(0.12))
                .with_animation(("area", id), animation(), |area, t| {
                    area.opacity(travelled(t) * (1. - 0.6 * spread(t)))
                })
        });
        let ring = div()
            .absolute()
            .left(relative(to.0))
            .top(relative(to.1))
            .rounded_full()
            .border_2()
            .border_color(colour)
            .with_animation(("ring", id), animation(), |ring, t| {
                let size = DOT + (RING - DOT) * spread(t);
                let seen = if t < arrival() { 0. } else { 1. - spread(t) };
                ring.size(px(size))
                    .ml(px(-size / 2.))
                    .mt(px(-size / 2.))
                    .opacity(seen * 0.8)
            });
        let dot = div()
            .absolute()
            .size(px(DOT))
            .ml(px(-DOT / 2.))
            .mt(px(-DOT / 2.))
            .rounded_full()
            .bg(colour)
            .border_2()
            .border_color(gpui::white())
            .shadow_md()
            .with_animation(("dot", id), animation(), move |dot, t| {
                let way = travelled(t);
                dot.left(relative(from.0 + (to.0 - from.0) * way))
                    .top(relative(from.1 + (to.1 - from.1) * way))
            });
        div()
            .absolute()
            .top_0()
            .left_0()
            .size_full()
            .children(area)
            .child(ring)
            .child(dot)
    }
}

impl Render for BrowserView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let docked = self.docked(window);
        // Floating, it covers the chat, so it waits for a page to show.
        if self.watching.is_none() || (!docked && self.frame.is_none()) {
            return div().into_any_element();
        }
        let doing = self.pointer.as_ref().map(|pointer| {
            div()
                .px_3()
                .py_2()
                .text_xs()
                .truncate()
                .text_color(cx.theme().muted_foreground)
                .child(pointer.doing.clone())
        });
        let panel = v_flex()
            .bg(cx.theme().background)
            .border_color(cx.theme().border)
            .child(self.header(cx))
            .child(self.screen(cx))
            .children(doing);
        if docked {
            panel
                .h_full()
                .w(relative(DOCK_SHARE))
                .min_w(DOCK_MIN)
                .max_w(DOCK_MAX)
                .flex_shrink_0()
                .border_l_1()
                .into_any_element()
        } else {
            panel
                .absolute()
                // Top right, clear of the composer and the newest messages.
                .top(px(48.))
                .right(px(16.))
                .w(FLOAT_WIDTH)
                .rounded(cx.theme().radius_lg)
                .border_1()
                .shadow_lg()
                .overflow_hidden()
                .into_any_element()
        }
    }
}

fn animation() -> Animation {
    Animation::new(POINTER_ANIMATION)
}

/// How far through the animation the pointer lands: when the action it
/// shows happens, since core waits that long before acting.
fn arrival() -> f32 {
    watch::POINTER_TIME.as_secs_f32() / POINTER_ANIMATION.as_secs_f32()
}

/// How far the pointer has gone on its way, `t` of the way through the
/// animation: quick at first, then settling onto its mark.
fn travelled(t: f32) -> f32 {
    let t = (t / arrival()).min(1.);
    1. - (1. - t).powi(4)
}

/// How far the click's ring has spread: nothing until the pointer lands.
fn spread(t: f32) -> f32 {
    ((t - arrival()) / (1. - arrival())).clamp(0., 1.)
}

/// A JPEG from the browser, as gpui draws a picture: blue before red.
fn picture(jpeg: &[u8]) -> Option<RenderImage> {
    let mut image = image::load_from_memory_with_format(jpeg, image::ImageFormat::Jpeg)
        .ok()?
        .into_rgba8();
    for pixel in image.pixels_mut() {
        pixel.0.swap(0, 2);
    }
    Some(RenderImage::new([image::Frame::new(image)]))
}

impl Chat {
    /// Show the browser when a call is about to use it: before it runs, or
    /// while it waits to be allowed, so the page can be seen first.
    pub(crate) fn show_browser(&mut self, tool: &str, cx: &mut Context<Self>) {
        if matches!(tool, "browse" | "use_page") {
            self.browser.update(cx, |view, cx| view.open(cx));
        }
    }
}

#[cfg(test)]
mod tests {
    // Not `use super::*`: the prelude re-exports gpui, whose `test` macro
    // would shadow the built-in attribute.
    use super::{arrival, picture, spread, travelled};

    #[test]
    fn the_pointer_lands_as_the_action_happens_and_the_ring_follows() {
        let landing = arrival();
        assert!(landing > 0.3 && landing < 0.7, "{landing}");
        assert_eq!(travelled(0.), 0.);
        assert!(travelled(landing * 0.5) > 0.5, "it eases out");
        assert_eq!(travelled(landing), 1.);
        assert_eq!(travelled(1.), 1.);
        assert_eq!(spread(landing), 0.);
        assert_eq!(spread(1.), 1.);
    }

    #[test]
    fn a_picture_comes_out_blue_before_red() {
        let mut red = image::RgbImage::new(2, 2);
        for pixel in red.pixels_mut() {
            pixel.0 = [250, 0, 0];
        }
        let mut jpeg = Vec::new();
        image::DynamicImage::ImageRgb8(red)
            .write_to(
                &mut std::io::Cursor::new(&mut jpeg),
                image::ImageFormat::Jpeg,
            )
            .unwrap();
        let frame = picture(&jpeg).unwrap();
        let bytes = frame.as_bytes(0).unwrap();
        assert!(bytes[2] > 200 && bytes[0] < 60, "{:?}", &bytes[..4]);
        assert!(picture(b"not a jpeg").is_none());
    }
}
