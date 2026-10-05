//! The browser beside the chat: the page the agent is using, live, with a
//! pointer that goes where the agent is about to act before it acts -- and
//! the user's to use too: an address bar, back and forward, and clicks,
//! keys and scrolling on the page go to it.
//!
//! Letronna has no web view of its own. These are the browser's pictures of
//! the page, which it sends only when the page changes -- at most thirty a
//! second, and none while the panel does not draw it -- drawn as images. A
//! browser with no window of its own lays the page out in the panel, at the
//! screen's sharpness, as a desktop's or a phone's. It is a tab of the right
//! panel (panel.rs), beside the chat, or over it in a window too narrow for
//! both -- or, while the agent browses, a small preview at the chat's
//! corner: the latest picture only, five a second at most, which a click
//! shows beside the chat.

use crate::prelude::*;
use crate::ui::panel::{PreviewBrowser, WideBrowser};
use gpui_component::Selectable;
use letronna_core::browser::PointedElement;
use letronna_core::watch::{self, Control, Held, Layout, Room, Sight, Watching};
use std::sync::Arc;
use std::time::{Duration, Instant};

actions!(browser, [PageTab, PageTabBack, CancelPoint]);

/// How far a notch of the wheel scrolls the page: a browser's own hundred
/// pixels, where the window counts three lines.
const WHEEL_LINE: Pixels = px(100. / 3.);
/// What the panel opens on when it is opened with no page: a search box,
/// as a browser opens on.
const HOME: &str = "https://www.google.com/";
/// How long a page asked to load gets to say it has started, before the
/// panel stops showing it loading.
const LOAD_WAIT: Duration = Duration::from_secs(5);
/// The least time the panel shows a page loading, so a reload that takes
/// no time is still seen to happen.
const LEAST_LOADING: Duration = Duration::from_millis(350);
/// One sweep of the bar that shows a page loading.
const LOADING_SWEEP: Duration = Duration::from_millis(1100);
/// How long the bar sweeps for a page that has not said it finished: one
/// still loading after this is more likely stuck than slow, and the sweep
/// draws the whole window at every refresh of the display.
const LOADING_SWEPT: Duration = Duration::from_secs(15);
/// The least time between the pictures the preview puts up: five a second
/// follows what the agent does, at a sixth of the browser's thirty.
const PREVIEW_GAP: Duration = Duration::from_millis(200);
/// The preview's picture is never shorter or taller than this, whatever the
/// page's shape: a strip says too little, and more covers the conversation.
const PREVIEW_SHORTEST: f64 = 140.;
const PREVIEW_TALLEST: f64 = 220.;
/// The zooms a browser steps through, smallest first.
const ZOOMS: &[f64] = &[0.5, 0.67, 0.75, 0.8, 0.9, 1., 1.1, 1.25, 1.5, 1.75, 2.];
/// How the keyboard's shortcuts are written here.
const SHORTCUT: &str = if cfg!(target_os = "macos") {
    "⌘"
} else {
    "Ctrl "
};
/// How long a pointer's move and its click take together.
const POINTER_ANIMATION: Duration = Duration::from_millis(900);
/// The pointer's dot, and the ring its click leaves at its widest.
const DOT: f32 = 14.;
const RING: f32 = 46.;

pub(crate) struct BrowserView {
    /// Read the browser's pictures, and everything else it shows, while
    /// the panel draws it. Dropping them stops the watching.
    watching: Option<[Task<()>; 2]>,
    /// Open, but watching nothing until the panel draws it again.
    unseen: bool,
    /// How the panel draws it, as it said last; None while it does not.
    drawn: Option<Drawn>,
    /// Closed by the user during this turn, so it stays closed until the
    /// next one.
    pub(crate) dismissed: bool,
    /// Given all the room beside the sidebar but the chat's, rather than
    /// the share of it the panel's edge was dragged to.
    pub(crate) wide: bool,
    frame: Option<Arc<RenderImage>>,
    /// Whether `frame` has been drawn. Until it is, the browser's tab or the
    /// window is out of sight, and newer pictures wait undecoded.
    frame_drawn: bool,
    /// The newest picture that came while `frame` was not drawn yet.
    waiting: Option<watch::Frame>,
    /// The page's size in CSS pixels, which pointers are given in.
    size: (f64, f64),
    url: String,
    /// The page's title, which the preview is headed with.
    title: String,
    pointer: Option<Pointer>,
    /// Pointers so far, so each gets an animation of its own.
    pointers: usize,
    /// The address bar, while an address is typed there.
    address: Entity<InputState>,
    /// Keys go to the page while it has this focus.
    focus: FocusHandle,
    /// And to the preview while it has this one.
    pub(crate) preview_focus: FocusHandle,
    /// Where the page is drawn, as laid out last.
    area: Option<Bounds<Pixels>>,
    /// The mouse button held down on the page, while it is.
    pressed: Option<watch::Button>,
    /// Since when the page is loading, while it is.
    loading: Option<Instant>,
    /// What the browser has said of loading so far, so a wait for it to
    /// say knows whether it has.
    loads: usize,
    /// Whether the tab has a page to go back to, and one to go forward to.
    history: (bool, bool),
    /// Whether the browser has a window of its own, and whether it is
    /// being given one or losing it.
    own_window: bool,
    switching: bool,
    /// The page's question, until it is answered.
    dialog: Option<String>,
    /// Since when the agent has waited on a site's check, which the user
    /// gets past on the page; it carries on by itself once it is gone.
    check: Option<Instant>,
    /// Why the address typed did not open.
    note: Option<String>,
    layout: Layout,
    zoom: f64,
    /// How far the wheel has turned with Ctrl held, towards the next zoom.
    zoom_wheel: f32,
    /// Point mode: off, waiting for a click, or an element chosen with its
    /// comment being typed.
    pointing: Option<Pointing>,
    _typing: Subscription,
}

/// Where the agent is about to act, in the page's CSS pixels.
struct Pointer {
    from: (f64, f64),
    to: (f64, f64),
    /// The box of what is acted on, until the page changes.
    area: Option<[f64; 4]>,
    doing: String,
}

/// Point mode's state: armed, waiting for a click on the page, or an
/// element chosen, its box highlighted while its comment is written.
enum Pointing {
    Armed,
    Chosen {
        element: PointedElement,
        comment: Entity<InputState>,
        _submit: Subscription,
    },
}

/// How the panel draws the browser.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Drawn {
    /// Beside the chat.
    Beside,
    /// In place of the conversation, in a room too narrow for both.
    Over,
    /// As the small preview at the chat's corner: a picture only.
    Preview,
}

/// A reference to something pointed at on the page, with the user's comment
/// on it, ready to go in the composer.
pub(crate) struct PointComment(pub(crate) String);

impl EventEmitter<PointComment> for BrowserView {}

/// Asked from the browser's bar: show it as the small preview instead.
pub(crate) struct ToPreview;

impl EventEmitter<ToPreview> for BrowserView {}

/// The agent has come to a site's check that only the user can get past:
/// the page wants showing where it can be clicked.
pub(crate) struct CheckUp;

impl EventEmitter<CheckUp> for BrowserView {}

impl BrowserView {
    pub(crate) fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let address = cx.new(|cx| InputState::new(window, cx).placeholder("Type an address"));
        let typing = cx.subscribe_in(&address, window, |view, _, event, window, cx| {
            if matches!(event, InputEvent::PressEnter { .. }) {
                view.visit(window, cx);
            }
        });
        // A browser started from now on draws as sharp as this screen.
        watch::screen(f64::from(window.scale_factor()));
        Self {
            watching: None,
            unseen: false,
            drawn: None,
            dismissed: false,
            wide: false,
            frame: None,
            frame_drawn: true,
            waiting: None,
            size: (0., 0.),
            url: String::new(),
            title: String::new(),
            pointer: None,
            pointers: 0,
            address,
            focus: cx.focus_handle(),
            preview_focus: cx.focus_handle().tab_stop(true),
            area: None,
            pressed: None,
            loading: None,
            loads: 0,
            history: (false, false),
            own_window: letronna_core::browser::has_window(),
            switching: false,
            dialog: None,
            check: None,
            note: None,
            layout: Layout::Landscape,
            zoom: 1.,
            zoom_wheel: 0.,
            pointing: None,
            _typing: typing,
        }
    }

    /// Start watching the browser, unless the panel was hidden this turn,
    /// or does not draw it now and will start it once it does.
    pub(crate) fn open(&mut self, cx: &mut Context<Self>) {
        if self.watching.is_some() || self.dismissed || self.unseen {
            return;
        }
        // A wait on a site's check that ended while nothing watched was
        // never told; one still going is told again as watching starts.
        self.check = None;
        let Watching { frames, sights } = watch::watch();
        let pictures = cx.spawn(async move |this, cx| {
            // When the last picture went up, which the preview's next waits on.
            let mut shown_at: Option<Instant> = None;
            while let Ok(mut frame) = frames.recv().await {
                // One that comes too soon for the preview waits its turn, and
                // gives way to any newer meanwhile: the browser drops what
                // the window is not ready for, so a picture held back unread
                // could leave the page's last look unseen.
                let gap = this
                    .read_with(cx, |view, _| view.picture_gap())
                    .unwrap_or_default();
                while let Some(left) = shown_at
                    .map(|at| gap.saturating_sub(at.elapsed()))
                    .filter(|left| !left.is_zero())
                {
                    match frames
                        .recv()
                        .with_timeout(left, cx.background_executor())
                        .await
                    {
                        Ok(Ok(newer)) => frame = newer,
                        Ok(Err(_)) => return,
                        Err(_) => break,
                    }
                }
                let frame = match this.update(cx, |view, _| view.decode_now(frame)) {
                    Ok(Some(frame)) => frame,
                    Ok(None) => continue,
                    Err(_) => break,
                };
                if decode(this.clone(), frame, cx).await.is_err() {
                    break;
                }
                shown_at = Some(Instant::now());
            }
        });
        let rest = cx.spawn(async move |this, cx| {
            while let Ok(sight) = sights.recv().await {
                let shown = this.update(cx, |view, cx| {
                    view.see(sight, cx);
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

    fn see(&mut self, sight: Sight, cx: &mut Context<Self>) {
        match sight {
            Sight::Pointer { x, y, area, doing } => self.point((x, y), area, doing),
            Sight::Page { title, url } => {
                // The box drawn, why an address did not open, and what was
                // pointed at were about the page before.
                if self.url != url {
                    if let Some(pointer) = self.pointer.as_mut() {
                        pointer.area = None;
                    }
                    self.note = None;
                    self.pointing = None;
                }
                self.url = url;
                self.title = title;
            }
            Sight::Loading(loading) => {
                self.loads += 1;
                match loading {
                    true => {
                        self.loading.get_or_insert_with(Instant::now);
                    }
                    false => self.finish_loading(cx),
                }
            }
            Sight::History { back, forward } => self.history = (back, forward),
            Sight::Dialog(asked) => self.dialog = asked,
            Sight::Copied(text) => cx.write_to_clipboard(ClipboardItem::new_string(text)),
            Sight::Closed => {
                self.show(None, cx);
                self.pointer = None;
                self.pointing = None;
                self.url.clear();
                self.title.clear();
                self.loading = None;
                self.history = (false, false);
                self.dialog = None;
                self.check = None;
            }
            Sight::Check(since) => {
                let began = since.is_some() && self.check.is_none();
                self.check = since;
                if began {
                    cx.emit(CheckUp);
                }
            }
        }
    }

    /// How many of the page's questions wait on the user.
    pub(crate) fn waiting(&self) -> usize {
        usize::from(self.dialog.is_some()) + usize::from(self.check.is_some())
    }

    /// Show the browser as the user asks for it: at once, even with no page
    /// yet, and then ready for an address.
    pub(crate) fn ask(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.dismissed = false;
        self.open(cx);
        if self.url.is_empty() {
            window.focus(&self.focus, cx);
        }
        if !letronna_core::browser::has_page() {
            self.resume(HOME.to_owned(), cx);
        }
    }

    /// Open the browser the user asked to see as the preview: as `ask` does,
    /// but leaving the keys where they are, and on `home` if the session's
    /// page is gone.
    pub(crate) fn reopen(&mut self, home: Option<String>, cx: &mut Context<Self>) {
        self.dismissed = false;
        self.open(cx);
        if !letronna_core::browser::has_page() {
            self.resume(home.unwrap_or(HOME.to_owned()), cx);
        }
    }

    /// Open the session's page again, closed with its browser while nothing
    /// used it, or `home` if it has none, showing it load meanwhile.
    fn resume(&mut self, home: String, cx: &mut Context<Self>) {
        self.note = None;
        self.start_loading(cx);
        cx.spawn(async move |view, cx| {
            let resumed = cx
                .background_executor()
                .spawn(async move { letronna_core::browser::resume(&home) })
                .await;
            let _ = view.update(cx, |view, cx| {
                match resumed {
                    Ok(()) => view.finish_loading(cx),
                    Err(why) => {
                        view.note = Some(why);
                        view.loading = None;
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Open: watching, or to watch again once the panel draws it.
    pub(crate) fn is_open(&self) -> bool {
        self.watching.is_some() || self.unseen
    }

    /// `frame`, to decode and put up now, unless the picture up already is
    /// not drawn yet: then it waits, the newest only, until it is.
    fn decode_now(&mut self, frame: watch::Frame) -> Option<watch::Frame> {
        if self.frame_drawn {
            self.waiting = None;
            Some(frame)
        } else {
            self.waiting = Some(frame);
            None
        }
    }

    pub(crate) fn close(&mut self, cx: &mut Context<Self>) {
        self.watching = None;
        self.waiting = None;
        self.frame_drawn = true;
        self.unseen = false;
        self.dismissed = true;
        self.area = None;
        self.pressed = None;
        self.pointing = None;
        self.show(None, cx);
        cx.notify();
    }

    /// Whether it stands as the panel drawing it so needs: told how, and
    /// watching only while drawn.
    pub(crate) fn is_drawn_as(&self, drawn: Option<Drawn>) -> bool {
        self.drawn == drawn && (drawn.is_some() || self.watching.is_none())
    }

    /// How the panel draws it now. Pictures nobody sees cost what seen ones
    /// do, so it stops watching while it is not drawn, and keeps its last
    /// picture and address for when it is again. Then it watches again,
    /// opening its page anew if the browser was closed meanwhile for being
    /// idle, which an unwatched one is after a while.
    pub(crate) fn set_drawn(&mut self, drawn: Option<Drawn>, cx: &mut Context<Self>) {
        self.drawn = drawn;
        // The preview is a picture: nothing on it is held or pointed at.
        if drawn == Some(Drawn::Preview) {
            self.pressed = None;
            self.pointing = None;
        }
        if drawn.is_none() {
            if self.watching.take().is_some() {
                self.unseen = true;
                self.pressed = None;
                self.pointing = None;
                // Unwatched, nothing says when the page has loaded.
                self.loading = None;
            }
        } else if std::mem::take(&mut self.unseen) {
            self.open(cx);
            if !self.url.is_empty() && !letronna_core::browser::has_page() {
                self.resume(HOME.to_owned(), cx);
            }
        }
        cx.notify();
    }

    /// How long a picture waits after the one before: the preview needs no
    /// more than a few a second to show what the agent does.
    fn picture_gap(&self) -> Duration {
        match self.drawn {
            Some(Drawn::Preview) => PREVIEW_GAP,
            _ => Duration::ZERO,
        }
    }

    /// What the preview is headed with: the page's title, else its site;
    /// None with no page open.
    pub(crate) fn page_name(&self) -> Option<String> {
        let title = self.title.trim();
        match self.url.as_str() {
            "" | "about:blank" => None,
            _ if !title.is_empty() => Some(title.to_owned()),
            // A file has no site: its whole address, then.
            url => Some(match site(url) {
                "" => url.to_owned(),
                site => site.to_owned(),
            }),
        }
    }

    /// What the agent is doing on the page, as its pointer last said.
    pub(crate) fn doing(&self) -> Option<String> {
        self.pointer
            .as_ref()
            .map(|pointer| pointer.doing.clone())
            .filter(|doing| !doing.is_empty())
    }

    /// How tall `thumbnail` draws the page `width` wide.
    pub(crate) fn thumbnail_height(&self, width: Pixels) -> Pixels {
        let page = match self.frame {
            Some(_) => self.size,
            None => (0., 0.),
        };
        px(preview_fit(f64::from(width), page).0 as f32)
    }

    /// The page as the preview shows it, `width` wide: the latest picture,
    /// filling the width and as tall as the page's shape gives within the
    /// preview's bounds, with the pointer over it.
    pub(crate) fn thumbnail(&self, width: Pixels, cx: &mut Context<Self>) -> AnyElement {
        let (page_width, page_height) = self.size;
        let picture = self
            .frame
            .clone()
            .filter(|_| page_width >= 1. && page_height >= 1.);
        let Some(frame) = picture else {
            return div()
                .w(width)
                .h(px(PREVIEW_SHORTEST as f32))
                .flex()
                .items_center()
                .justify_center()
                .px_4()
                .text_xs()
                .text_center()
                .text_color(cx.theme().muted_foreground)
                .child("The page shows here as the agent browses.")
                .into_any_element();
        };
        let (height, left, scale) = preview_fit(f64::from(width), self.size);
        div()
            .relative()
            .flex_shrink_0()
            .w(width)
            .h(px(height as f32))
            .overflow_hidden()
            .child(
                div()
                    .absolute()
                    .top_0()
                    .left(px(left as f32))
                    .w(px((page_width * scale) as f32))
                    .h(px((page_height * scale) as f32))
                    .child(img(frame).size_full())
                    .children(
                        self.pointer
                            .as_ref()
                            .map(|pointer| self.pointer_over(pointer, cx)),
                    ),
            )
            .into_any_element()
    }

    /// Put up a new picture, and give the one before back to the GPU: gpui
    /// keeps every picture it has drawn until told it is done with it, and a
    /// page that moves would fill the atlas with them.
    fn show(&mut self, frame: Option<Arc<RenderImage>>, cx: &mut Context<Self>) {
        if let Some(old) = std::mem::replace(&mut self.frame, frame) {
            // Once the window is free: while it is being updated -- a click
            // closing the panel -- gpui leaves it out, and the picture stays.
            cx.defer(move |cx| cx.drop_image(old, None));
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

    /// Open what was typed in the address bar, starting the browser if it
    /// is not running; keys go to the page after.
    fn visit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let typed = self.address.read(cx).value().trim().to_owned();
        if typed.is_empty() {
            return;
        }
        window.focus(&self.focus, cx);
        self.open_address(typed, cx);
    }

    /// Open an address, starting the browser if it is not running.
    fn open_address(&mut self, address: String, cx: &mut Context<Self>) {
        self.note = None;
        self.start_loading(cx);
        cx.spawn(async move |view, cx| {
            let opened = cx
                .background_executor()
                .spawn(async move { letronna_core::browser::visit(&address) })
                .await;
            let _ = view.update(cx, |view, cx| {
                if let Err(why) = opened {
                    view.note = Some(why);
                    view.loading = None;
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Show the page loading as soon as it is asked to, not once the
    /// browser says so -- and stop, if the browser never does.
    fn start_loading(&mut self, cx: &mut Context<Self>) {
        self.loading.get_or_insert_with(Instant::now);
        let heard = self.loads;
        cx.spawn(async move |view, cx| {
            cx.background_executor().timer(LOAD_WAIT).await;
            let _ = view.update(cx, |view, cx| {
                if view.loads == heard {
                    view.loading = None;
                    cx.notify();
                }
            });
        })
        .detach();
        cx.notify();
    }

    /// Stop showing the page loading: after a moment, if it only just
    /// started, so a quick reload is still seen.
    fn finish_loading(&mut self, cx: &mut Context<Self>) {
        let left = self
            .loading
            .map(|since| LEAST_LOADING.saturating_sub(since.elapsed()))
            .unwrap_or_default();
        if left.is_zero() {
            self.loading = None;
            return;
        }
        let heard = self.loads;
        cx.spawn(async move |view, cx| {
            cx.background_executor().timer(left).await;
            let _ = view.update(cx, |view, cx| {
                if view.loads == heard {
                    view.loading = None;
                    cx.notify();
                }
            });
        })
        .detach();
    }

    /// Give the browser a window of its own, or take it back into the
    /// panel, with the page it has open.
    fn switch_window(&mut self, cx: &mut Context<Self>) {
        let shown = !self.own_window;
        self.switching = true;
        self.note = None;
        self.start_loading(cx);
        cx.spawn(async move |view, cx| {
            let switched = cx
                .background_executor()
                .spawn(async move { letronna_core::browser::give_window(shown) })
                .await;
            let _ = view.update(cx, |view, cx| {
                view.switching = false;
                view.own_window = letronna_core::browser::has_window();
                if let Err(why) = switched {
                    view.note = Some(why);
                    view.loading = None;
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Where the page is, over the address bar while nobody types in it:
    /// from its start, where the site is named, and without the https every
    /// page has. Clicked, the bar beneath takes the whole address, chosen,
    /// to type over.
    fn shown_address(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let shown = match self.url.as_str() {
            "" | "about:blank" => None,
            url => Some(url.strip_prefix("https://").unwrap_or(url).to_owned()),
        };
        div()
            .id("page-address")
            .absolute()
            .inset_0()
            .px_1()
            .flex()
            .items_center()
            .occlude()
            .bg(cx.theme().background)
            .rounded(cx.theme().radius)
            .border_1()
            .border_color(cx.theme().input)
            .text_xs()
            .cursor_text()
            .child(div().min_w_0().truncate().map(|text| {
                match shown {
                    Some(url) => text.child(url),
                    None => text
                        .text_color(cx.theme().muted_foreground)
                        .child("Type an address"),
                }
            }))
            .on_click(cx.listener(|view, _, window, cx| {
                let url = match view.url.as_str() {
                    "about:blank" => String::new(),
                    url => url.to_owned(),
                };
                view.address.update(cx, |address, cx| {
                    address.set_value(url, window, cx);
                    address.focus(window, cx);
                });
                // The bar is drawn beneath this, so it takes the action now.
                window.dispatch_action(Box::new(gpui_component::input::SelectAll), cx);
                cx.notify();
            }))
    }

    /// Where the page was drawn this time, which the browser lays it out
    /// in, and clicks are placed by.
    fn placed(&mut self, bounds: Bounds<Pixels>, window: &mut Window) {
        if self.area == Some(bounds) {
            return;
        }
        self.area = Some(bounds);
        self.fit_page(window);
        // The picture is placed by the area, so once more with it.
        window.request_animation_frame();
    }

    /// Tell the browser the room the page has, and how it is laid out and
    /// zoomed there.
    fn fit_page(&self, window: &Window) {
        let Some(bounds) = self.area else {
            return;
        };
        watch::fit(Room {
            width: f64::from(bounds.size.width),
            height: f64::from(bounds.size.height),
            scale: f64::from(window.scale_factor()),
            fit: true,
            layout: self.layout,
            zoom: self.zoom,
        });
    }

    /// Lay the page out as a desktop browser does, or as a phone does.
    fn set_layout(&mut self, layout: Layout, window: &mut Window, cx: &mut Context<Self>) {
        if layout == self.layout {
            return;
        }
        self.layout = layout;
        self.fit_page(window);
        cx.notify();
    }

    fn set_zoom(&mut self, zoom: f64, window: &mut Window, cx: &mut Context<Self>) {
        self.zoom = zoom;
        self.fit_page(window);
        cx.notify();
    }

    /// A point in the window as the point on the page under it.
    fn on_page(&self, at: Point<Pixels>) -> Option<(f64, f64)> {
        let area = self.area?;
        let (left, top, scale) = fitted(size_of(area), self.size)?;
        let x = (f64::from(at.x - area.origin.x) - left) / scale;
        let y = (f64::from(at.y - area.origin.y) - top) / scale;
        Some((x.clamp(0., self.size.0), y.clamp(0., self.size.1)))
    }

    fn mouse(
        &mut self,
        event: watch::Mouse,
        button: MouseButton,
        at: Point<Pixels>,
        clicks: usize,
        modifiers: Modifiers,
    ) {
        let button = match button {
            MouseButton::Left => watch::Button::Left,
            MouseButton::Middle => watch::Button::Middle,
            MouseButton::Right => watch::Button::Right,
            _ => return,
        };
        let Some((x, y)) = self.on_page(at) else {
            return;
        };
        self.pressed = (event == watch::Mouse::Down).then_some(button);
        watch::control(Control::Mouse {
            event,
            x,
            y,
            button,
            clicks,
            held: held(modifiers),
        });
    }

    /// Turn Point mode on -- click a spot on the page to point at what is
    /// there and comment on it, for the composer -- or off.
    fn toggle_point(&mut self, cx: &mut Context<Self>) {
        if self.pointing.is_none() && (self.url.is_empty() || self.url == "about:blank") {
            return;
        }
        self.pointing = match self.pointing {
            None => Some(Pointing::Armed),
            Some(_) => None,
        };
        cx.notify();
    }

    fn cancel_point(&mut self, cx: &mut Context<Self>) {
        self.pointing = None;
        cx.notify();
    }

    /// Escape while pointing cancels it here, before it reaches Chat's own
    /// Escape; otherwise it is nothing nearer took, and goes on to Chat's.
    fn on_cancel_point_key(&mut self, cx: &mut Context<Self>) {
        match self.pointing {
            Some(_) => self.cancel_point(cx),
            None => cx.propagate(),
        }
    }

    /// A click on the page while Point mode is armed: look up what is there,
    /// mapped the same way a real click on the page is, and ask for a
    /// comment on it.
    fn point_click(&mut self, at: Point<Pixels>, window: &mut Window, cx: &mut Context<Self>) {
        let Some((x, y)) = self.on_page(at) else {
            return;
        };
        cx.spawn_in(window, async move |view, cx| {
            let found = cx
                .background_executor()
                .spawn(async move { letronna_core::browser::point_at(x, y) })
                .await;
            let _ = view.update_in(cx, |view, window, cx| {
                // Only while still waiting for it: Escape, or a first click's
                // answer, may have come in the meantime.
                match found {
                    Ok(element) if matches!(view.pointing, Some(Pointing::Armed)) => {
                        view.choose_point(element, window, cx)
                    }
                    Ok(_) => {}
                    Err(why) => view.note = Some(why),
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// What was pointed at: highlight its box, and ask for a comment on it.
    fn choose_point(
        &mut self,
        element: PointedElement,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let comment = cx.new(|cx| InputState::new(window, cx).placeholder("Comment on this…"));
        let submit = cx.subscribe_in(&comment, window, |view, _, event, _, cx| {
            if matches!(event, InputEvent::PressEnter { .. }) {
                view.submit_point(cx);
            }
        });
        comment.update(cx, |input, cx| input.focus(window, cx));
        self.pointing = Some(Pointing::Chosen {
            element,
            comment,
            _submit: submit,
        });
        cx.notify();
    }

    /// Add what was pointed at, with the comment, to the composer.
    fn submit_point(&mut self, cx: &mut Context<Self>) {
        let Some(Pointing::Chosen {
            element, comment, ..
        }) = &self.pointing
        else {
            return;
        };
        let text = comment.read(cx).value().trim().to_owned();
        if text.is_empty() {
            return;
        }
        let reference = letronna_core::browser::point_reference(element, &text);
        self.pointing = None;
        cx.emit(PointComment(reference));
        cx.notify();
    }

    fn key_down(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        // The comment box sits inside the page: held here, what is typed
        // into it would never reach it.
        if let Some(Pointing::Chosen { comment, .. }) = &self.pointing
            && comment.focus_handle(cx).is_focused(window)
        {
            return;
        }
        let stroke = &event.keystroke;
        let shortcut = stroke.modifiers.secondary();
        match stroke.key.as_str() {
            "=" | "+" if shortcut => self.set_zoom(zoom_step(self.zoom, true), window, cx),
            "-" if shortcut => self.set_zoom(zoom_step(self.zoom, false), window, cx),
            "0" if shortcut => self.set_zoom(1., window, cx),
            "p" if shortcut => self.toggle_point(cx),
            // Clicks are held back while pointing, so keys are too: Enter on
            // a focused page button would click it all the same.
            _ if self.pointing.is_some() => {}
            "c" if shortcut => watch::control(Control::Copy),
            "x" if shortcut => {
                watch::control(Control::Copy);
                press("delete", None, Held::default());
            }
            "v" if shortcut => {
                if let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) {
                    watch::control(Control::Paste(text));
                }
            }
            name => press(name, stroke.key_char.clone(), held(stroke.modifiers)),
        }
        cx.stop_propagation();
    }

    fn header(&self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // A way to go somewhere, which shows the page loading at once.
        let go = |id: &'static str, icon: Icon, tooltip: &'static str, to: fn() -> Control| {
            Button::new(id)
                .plain(cx)
                .xsmall()
                .icon(icon)
                .tooltip(tooltip)
                .on_click(cx.listener(move |view, _, _, cx| {
                    view.start_loading(cx);
                    watch::control(to());
                }))
        };
        let (back, forward) = self.history;
        let reload = match self.loading {
            Some(_) => Button::new("page-stop")
                .plain(cx)
                .xsmall()
                .icon(Icon::new(IconName::Close))
                .tooltip("Stop loading")
                .on_click(|_, _, _| watch::control(Control::Stop)),
            None => go("page-reload", Icon::new(AppIcon::Reload), "Reload", || {
                Control::Reload
            })
            .disabled(self.url.is_empty()),
        };
        let view = cx.entity();
        let (layout, zoom) = (self.layout, self.zoom);
        let view_menu = Button::new("page-view")
            .plain(cx)
            .xsmall()
            .icon(Icon::new(AppIcon::View))
            .dropdown_menu_with_anchor(gpui::Anchor::TopRight, move |menu, _, _| {
                let act = |apply: fn(&mut BrowserView, &mut Window, &mut Context<BrowserView>)| {
                    let view = view.clone();
                    move |_: &ClickEvent, window: &mut Window, cx: &mut App| {
                        view.update(cx, |view, cx| apply(view, window, cx))
                    }
                };
                menu.min_w(px(230.))
                    .item(
                        PopupMenuItem::new("Desktop layout")
                            .checked(layout == Layout::Landscape)
                            .on_click(act(|view, window, cx| {
                                view.set_layout(Layout::Landscape, window, cx)
                            })),
                    )
                    .item(
                        PopupMenuItem::new("Phone layout")
                            .checked(layout == Layout::Portrait)
                            .on_click(act(|view, window, cx| {
                                view.set_layout(Layout::Portrait, window, cx)
                            })),
                    )
                    .separator()
                    .item(
                        PopupMenuItem::new(format!("Zoom in  {SHORTCUT}+"))
                            .disabled(zoom_step(zoom, true) == zoom)
                            .on_click(act(|view, window, cx| {
                                view.set_zoom(zoom_step(view.zoom, true), window, cx)
                            })),
                    )
                    .item(
                        PopupMenuItem::new(format!("Zoom out  {SHORTCUT}-"))
                            .disabled(zoom_step(zoom, false) == zoom)
                            .on_click(act(|view, window, cx| {
                                view.set_zoom(zoom_step(view.zoom, false), window, cx)
                            })),
                    )
                    .item(
                        PopupMenuItem::new(format!("Actual size  {SHORTCUT}0"))
                            .checked((zoom - 1.).abs() < 0.001)
                            .on_click(act(|view, window, cx| view.set_zoom(1., window, cx))),
                    )
            });
        // The zoom, while it is not the page's own size; clicked, it is again.
        let zoomed = ((self.zoom - 1.).abs() > 0.001).then(|| {
            Button::new("page-zoom")
                .plain(cx)
                .xsmall()
                .label(format!("{:.0}%", self.zoom * 100.))
                .tooltip("Back to actual size")
                .on_click(cx.listener(|view, _, window, cx| view.set_zoom(1., window, cx)))
        });
        let point_button = Button::new("page-point")
            .plain(cx)
            .xsmall()
            .icon(Icon::new(AppIcon::Point))
            .selected(self.pointing.is_some())
            .tooltip(format!("Point at something and comment  {SHORTCUT}P"))
            .disabled(self.url.is_empty() || self.url == "about:blank")
            .on_click(cx.listener(|view, _, _, cx| view.toggle_point(cx)));
        let own_window = Button::new("page-window")
            .plain(cx)
            .xsmall()
            .map(|button| match self.own_window {
                true => button
                    .icon(Icon::new(AppIcon::PopIn))
                    .tooltip("Show it only here"),
                false => button
                    .icon(Icon::new(AppIcon::PopOut))
                    .tooltip("Open in its own window"),
            })
            .disabled(self.switching)
            .on_click(cx.listener(|view, _, _, cx| view.switch_window(cx)));
        let shrink = Button::new("page-preview")
            .plain(cx)
            .xsmall()
            .icon(Icon::new(AppIcon::Preview))
            .tooltip_with_action("Shrink to a preview", &PreviewBrowser, Some("Chat"))
            .on_click(cx.listener(|_, _, _, cx| cx.emit(ToPreview)));
        // Beside the chat only: over it, the page has all the room there is.
        let wide = (self.drawn == Some(Drawn::Beside)).then(|| {
            Button::new("page-wide")
                .plain(cx)
                .xsmall()
                .map(|button| match self.wide {
                    true => button.icon(Icon::new(AppIcon::Narrow)).tooltip_with_action(
                        "Back beside the chat",
                        &WideBrowser,
                        Some("Chat"),
                    ),
                    false => button.icon(Icon::new(AppIcon::Widen)).tooltip_with_action(
                        "Give the page the most room",
                        &WideBrowser,
                        Some("Chat"),
                    ),
                })
                .on_click(cx.listener(|view, _, _, cx| {
                    view.wide = !view.wide;
                    cx.notify();
                }))
        });
        let typing = self.address.read(cx).focus_handle(cx).is_focused(window);
        h_flex()
            .h(px(38.))
            .flex_shrink_0()
            .px_2()
            .gap_1()
            .items_center()
            .border_b_1()
            .border_color(cx.theme().border)
            .child(
                go("page-back", Icon::new(IconName::ArrowLeft), "Back", || {
                    Control::Back
                })
                .disabled(!back),
            )
            .child(
                go(
                    "page-forward",
                    Icon::new(IconName::ArrowRight),
                    "Forward",
                    || Control::Forward,
                )
                .disabled(!forward),
            )
            .child(reload)
            .child(
                div()
                    .relative()
                    .flex_1()
                    .min_w_0()
                    .font_family(MONO_FONT)
                    .child(Input::new(&self.address).xsmall())
                    .when(!typing, |bar| bar.child(self.shown_address(cx))),
            )
            .children(zoomed)
            .child(point_button)
            .child(view_menu)
            .child(shrink)
            .children(wide)
            .child(own_window)
    }

    /// The page, with the pointer over it, which takes the user's clicks,
    /// keys and scrolling. It fills the panel, and the page is laid out to
    /// fit.
    fn screen(&self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let view = cx.entity();
        let measure = canvas(
            move |bounds, window, cx| {
                view.update(cx, |view, _| view.placed(bounds, window));
            },
            |_, _, _, _| {},
        )
        .absolute()
        .size_full();
        let focused = self.focus.is_focused(window);
        let mut screen = div()
            .id("page")
            .relative()
            .w_full()
            .flex_1()
            .min_h_0()
            .overflow_hidden()
            .track_focus(&self.focus)
            .key_context("Browser")
            .on_key_down(cx.listener(Self::key_down))
            .on_action(cx.listener(|_, _: &PageTab, _, _| {
                press("tab", None, Held::default());
            }))
            .on_action(cx.listener(|_, _: &PageTabBack, _, _| {
                let shift = Held {
                    shift: true,
                    ..Held::default()
                };
                press("tab", None, shift);
            }))
            .on_action(cx.listener(|view, _: &CancelPoint, _, cx| view.on_cancel_point_key(cx)))
            .when(matches!(self.pointing, Some(Pointing::Armed)), |screen| {
                screen.cursor(CursorStyle::Crosshair)
            })
            .on_scroll_wheel(cx.listener(|view, event: &ScrollWheelEvent, window, cx| {
                let delta = event.delta.pixel_delta(WHEEL_LINE);
                if event.modifiers.secondary() {
                    // With Ctrl the wheel zooms, as in a browser: a step a
                    // notch, however finely a touchpad turns it.
                    view.zoom_wheel += f32::from(delta.y);
                    let notch = f32::from(WHEEL_LINE) * 3.;
                    if view.zoom_wheel.abs() >= notch {
                        let larger = view.zoom_wheel > 0.;
                        view.zoom_wheel = 0.;
                        view.set_zoom(zoom_step(view.zoom, larger), window, cx);
                    }
                } else if let Some((x, y)) = view.on_page(event.position) {
                    watch::control(Control::Wheel {
                        x,
                        y,
                        dx: -f64::from(delta.x),
                        dy: -f64::from(delta.y),
                        held: held(event.modifiers),
                    });
                }
                cx.stop_propagation();
            }))
            .on_mouse_move(cx.listener(|view, event: &MouseMoveEvent, _, _| {
                if let Some((x, y)) = view.on_page(event.position) {
                    watch::control(Control::Mouse {
                        event: watch::Mouse::Moved,
                        x,
                        y,
                        button: view.pressed.unwrap_or(watch::Button::None),
                        clicks: 0,
                        held: held(event.modifiers),
                    });
                }
            }))
            // A drag let go of outside the page still ends on it.
            .on_mouse_up_out(
                MouseButton::Left,
                cx.listener(|view, event: &MouseUpEvent, _, _| {
                    if view.pressed.is_some() {
                        view.mouse(
                            watch::Mouse::Up,
                            event.button,
                            event.position,
                            1,
                            event.modifiers,
                        );
                    }
                }),
            )
            .child(measure);
        for button in [MouseButton::Left, MouseButton::Middle, MouseButton::Right] {
            screen = screen
                .on_mouse_down(
                    button,
                    cx.listener(|view, event: &MouseDownEvent, window, cx| {
                        window.focus(&view.focus, cx);
                        if event.button == MouseButton::Left
                            && matches!(view.pointing, Some(Pointing::Armed))
                        {
                            view.point_click(event.position, window, cx);
                        } else if view.pointing.is_none() {
                            view.mouse(
                                watch::Mouse::Down,
                                event.button,
                                event.position,
                                event.click_count,
                                event.modifiers,
                            );
                        }
                        cx.stop_propagation();
                    }),
                )
                .on_mouse_up(
                    button,
                    cx.listener(|view, event: &MouseUpEvent, _, _| {
                        if view.pointing.is_none() {
                            view.mouse(
                                watch::Mouse::Up,
                                event.button,
                                event.position,
                                event.click_count,
                                event.modifiers,
                            );
                        }
                    }),
                );
        }
        let (width, height) = self.size;
        let picture = self.frame.clone().filter(|_| width >= 1. && height >= 1.);
        let placed = self.area.and_then(|area| fitted(size_of(area), self.size));
        let screen = match (picture, placed) {
            (Some(frame), Some((left, top, scale))) => screen.child(
                div()
                    .absolute()
                    .left(px(left as f32))
                    .top(px(top as f32))
                    .w(px((width * scale) as f32))
                    .h(px((height * scale) as f32))
                    .child(img(frame).size_full())
                    .children(self.point_area().map(|area| self.point_box(area, cx)))
                    .children(
                        self.pointer
                            .as_ref()
                            .map(|pointer| self.pointer_over(pointer, cx)),
                    ),
            ),
            _ => screen.child(
                div()
                    .size_full()
                    .flex()
                    .items_center()
                    .justify_center()
                    .px_6()
                    .child(
                        div()
                            .w_full()
                            .text_xs()
                            .text_center()
                            .text_color(cx.theme().muted_foreground)
                            .child(
                                "The page shows here while the agent uses the browser, or type an \
                             address above.",
                            ),
                    ),
            ),
        };
        screen
            .children(
                (self.loading)
                    .filter(|since| since.elapsed() < LOADING_SWEPT)
                    .map(|_| loading_bar(cx)),
            )
            .children(self.overlay(cx))
            // Lit while keys go to the page; drawn over it, so the page
            // does not move.
            .when(focused, |screen| {
                screen.child(
                    div()
                        .absolute()
                        .inset_0()
                        .border_1()
                        .border_color(cx.theme().ring),
                )
            })
            .into_any_element()
    }

    /// What is said over the page: its question, why an address did not
    /// open, and what the agent is doing. Over it, not beside it, so the
    /// page keeps its size.
    fn overlay(&self, cx: &mut Context<Self>) -> Option<impl IntoElement> {
        let card = || {
            h_flex()
                .gap_2()
                .items_center()
                .px_3()
                .py_2()
                .rounded(cx.theme().radius)
                .bg(cx.theme().popover)
                .border_1()
                .border_color(cx.theme().border)
                .text_xs()
        };
        let asked = self.dialog.as_ref().map(|asked| {
            let answer = |agree: bool| {
                cx.listener(move |view: &mut Self, _, _, cx| {
                    watch::control(Control::Answer(agree));
                    view.dialog = None;
                    cx.notify();
                })
            };
            card()
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .child(format!("The page asks: {asked}")),
                )
                .child(
                    Button::new("page-ok")
                        .primary()
                        .xsmall()
                        .label("OK")
                        .on_click(answer(true)),
                )
                .child(
                    Button::new("page-cancel")
                        .plain(cx)
                        .xsmall()
                        .label("Cancel")
                        .on_click(answer(false)),
                )
        });
        let note = self.note.as_ref().map(|note| {
            card()
                .text_color(cx.theme().danger)
                .child(div().flex_1().min_w_0().child(note.clone()))
        });
        let check = self.check.map(|_| {
            card().child(
                div()
                    .flex_1()
                    .min_w_0()
                    .child("Get past the site's check on the page; Letronna carries on by itself."),
            )
        });
        let doing = self.pointer.as_ref().map(|pointer| {
            div()
                .px_3()
                .py_1()
                .truncate()
                .text_xs()
                .bg(cx.theme().background.opacity(0.88))
                .text_color(cx.theme().muted_foreground)
                .child(pointer.doing.clone())
        });
        let hint = matches!(self.pointing, Some(Pointing::Armed)).then(|| {
            div()
                .px_3()
                .py_1()
                .truncate()
                .text_xs()
                .bg(cx.theme().background.opacity(0.88))
                .text_color(cx.theme().muted_foreground)
                .child("Click something on the page to point at it.")
        });
        let commenting = match &self.pointing {
            Some(Pointing::Chosen {
                element, comment, ..
            }) => Some(
                card()
                    .items_start()
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .gap_1()
                            .child(
                                div()
                                    .truncate()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(format!("Pointing at {}", element.selector)),
                            )
                            .child(Input::new(comment).xsmall()),
                    )
                    .child(
                        Button::new("point-submit")
                            .primary()
                            .xsmall()
                            .icon(Icon::new(IconName::Check))
                            .tooltip("Add to the composer")
                            .on_click(cx.listener(|view, _, _, cx| view.submit_point(cx))),
                    )
                    .child(
                        Button::new("point-cancel")
                            .plain(cx)
                            .xsmall()
                            .icon(Icon::new(IconName::Close))
                            .tooltip("Cancel")
                            .on_click(cx.listener(|view, _, _, cx| view.cancel_point(cx))),
                    ),
            ),
            _ => None,
        };
        if asked.is_none()
            && check.is_none()
            && note.is_none()
            && doing.is_none()
            && hint.is_none()
            && commenting.is_none()
        {
            return None;
        }
        // At the top the card would cover what it is about when that sits
        // in the page's upper half, as headings and menus do.
        let low = self
            .point_area()
            .is_some_and(|[_, y, _, h]| y + h / 2. < self.size.1 / 2.);
        let (above, below) = if low {
            (None, commenting)
        } else {
            (commenting, None)
        };
        Some(
            v_flex()
                .absolute()
                .inset_0()
                .justify_between()
                .child(
                    v_flex()
                        .p_2()
                        .gap_2()
                        .children(asked)
                        .children(check)
                        .children(note)
                        .children(above),
                )
                .children(below.map(|card| div().p_2().child(card)))
                .children(doing)
                .children(hint),
        )
    }

    /// The box of what was pointed at, in CSS pixels, while its comment is
    /// being written.
    fn point_area(&self) -> Option<[f64; 4]> {
        match &self.pointing {
            Some(Pointing::Chosen { element, .. }) => Some(element.area),
            _ => None,
        }
    }

    /// A static highlight over what was pointed at, until the comment is
    /// done or cancelled.
    fn point_box(&self, area: [f64; 4], cx: &mut Context<Self>) -> impl IntoElement {
        let (width, height) = self.size;
        let [x, y, w, h] = area;
        div()
            .absolute()
            .left(relative((x / width) as f32))
            .top(relative((y / height) as f32))
            .w(relative((w / width) as f32))
            .h(relative((h / height) as f32))
            .rounded(px(4.))
            .border_2()
            .border_color(cx.theme().primary)
            .bg(cx.theme().primary.opacity(0.12))
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
        self.frame_drawn = true;
        // Back in sight: the newest picture that came meanwhile goes up, and
        // newer ones wait for it to be drawn, so they go up in order.
        if let Some(frame) = self.waiting.take() {
            self.frame_drawn = false;
            cx.spawn(async move |this, cx| {
                let _ = decode(this, frame, cx).await;
            })
            .detach();
        }
        v_flex()
            // The page fills what the panel has left under the bar.
            .flex_1()
            .min_h_0()
            .child(self.header(window, cx))
            .child(self.screen(window, cx))
    }
}

/// Decode `frame` off the UI thread and put it up in the view; an error
/// once the view is gone.
async fn decode(
    view: WeakEntity<BrowserView>,
    frame: watch::Frame,
    cx: &mut AsyncApp,
) -> Result<(), ()> {
    let (width, height) = (frame.width, frame.height);
    let decode = cx
        .background_executor()
        .spawn(async move { picture(&frame.image) });
    let image = decode.await;
    view.update(cx, |view, cx| match image {
        Some(image) => {
            view.show(Some(Arc::new(image)), cx);
            view.size = (width, height);
            view.frame_drawn = false;
            cx.notify();
        }
        // Nothing went up, so the next picture need not wait for it.
        None => view.frame_drawn = true,
    })
    .map_err(|_| ())
}

/// A bar sweeping across the top of the page while it loads.
fn loading_bar(cx: &mut Context<BrowserView>) -> impl IntoElement {
    div()
        .absolute()
        .top_0()
        .left_0()
        .right_0()
        .h(px(2.))
        .overflow_hidden()
        .child(
            div()
                .absolute()
                .top_0()
                .h_full()
                .w(relative(0.3))
                .bg(cx.theme().primary)
                .with_animation(
                    "page-loading",
                    Animation::new(LOADING_SWEEP)
                        .repeat()
                        .with_easing(ease_in_out),
                    |bar, t| bar.left(relative(-0.3 + 1.3 * t)),
                ),
        )
}

/// Press a key on the page, by the window's name for it.
fn press(name: &str, text: Option<String>, held: Held) {
    watch::control(Control::Key {
        name: name.to_owned(),
        text,
        held,
    });
}

fn held(modifiers: Modifiers) -> Held {
    Held {
        alt: modifiers.alt,
        control: modifiers.control,
        meta: modifiers.platform,
        shift: modifiers.shift,
    }
}

fn size_of(area: Bounds<Pixels>) -> (f64, f64) {
    (f64::from(area.size.width), f64::from(area.size.height))
}

/// Where a picture of a page `page` big sits in an area `area` big: as
/// large as fits, across the middle and at the top. Its left and top, and
/// how many of the area's pixels are one of the page's.
fn fitted(area: (f64, f64), page: (f64, f64)) -> Option<(f64, f64, f64)> {
    if page.0 < 1. || page.1 < 1. || area.0 < 1. || area.1 < 1. {
        return None;
    }
    let scale = (area.0 / page.0).min(area.1 / page.1);
    Some(((area.0 - page.0 * scale) / 2., 0., scale))
}

/// How tall the preview's picture is `width` wide, and where a page `page`
/// big sits in it: filling it, cut at the bottom when the page is taller
/// than the preview goes, and at the sides when wider. Its height, the
/// page's left, and how many of the preview's pixels are one of the page's.
fn preview_fit(width: f64, page: (f64, f64)) -> (f64, f64, f64) {
    if page.0 < 1. || page.1 < 1. || width < 1. {
        return (PREVIEW_SHORTEST, 0., 0.);
    }
    let height = (width * page.1 / page.0).clamp(PREVIEW_SHORTEST, PREVIEW_TALLEST);
    let scale = (width / page.0).max(height / page.1);
    (height, (width - page.0 * scale) / 2., scale)
}

/// The site an address is on: `example.com` of `https://example.com/a?b`.
fn site(url: &str) -> &str {
    let rest = url.split_once("://").map_or(url, |(_, rest)| rest);
    rest.split(['/', '?', '#']).next().unwrap_or(rest)
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

/// The next zoom from `zoom`, larger or smaller, as far as the steps go.
fn zoom_step(zoom: f64, larger: bool) -> f64 {
    let next = match larger {
        true => ZOOMS.iter().find(|step| **step > zoom + 0.001),
        false => ZOOMS.iter().rev().find(|step| **step < zoom - 0.001),
    };
    next.copied().unwrap_or(zoom)
}

/// A picture from the browser, JPEG or PNG, as gpui draws one: blue
/// before red.
fn picture(bytes: &[u8]) -> Option<RenderImage> {
    let mut image = image::load_from_memory(bytes).ok()?.into_rgba8();
    for pixel in image.pixels_mut() {
        pixel.0.swap(0, 2);
    }
    Some(RenderImage::new([image::Frame::new(image)]))
}

#[cfg(test)]
mod tests {
    // Not `use super::*`: the prelude re-exports gpui, whose `test` macro
    // would shadow the built-in attribute.
    use super::{arrival, fitted, picture, preview_fit, site, spread, travelled, zoom_step};

    #[test]
    fn zoom_steps_as_a_browser_does_and_stops_at_the_ends() {
        assert_eq!(zoom_step(1., true), 1.1);
        assert_eq!(zoom_step(1., false), 0.9);
        assert_eq!(zoom_step(1.25, true), 1.5);
        assert_eq!(zoom_step(2., true), 2.);
        assert_eq!(zoom_step(0.5, false), 0.5);
    }

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
    fn the_page_fills_the_panel_it_was_laid_out_for_and_fits_one_it_was_not() {
        // Laid out in the panel: one of its pixels to one of the page's.
        assert_eq!(fitted((480., 900.), (480., 900.)), Some((0., 0., 1.)));
        // A page wider than the panel shows smaller, at the top, across it.
        assert_eq!(fitted((320., 400.), (1280., 800.)), Some((0., 0., 0.25)));
        // A page taller than the panel is centred across it.
        assert_eq!(fitted((500., 400.), (400., 800.)), Some((150., 0., 0.5)));
        assert_eq!(fitted((500., 400.), (0., 0.)), None);
    }

    #[test]
    fn the_preview_keeps_the_page_shape_within_its_bounds() {
        // A desktop page, wider than tall, fills the preview's width whole.
        assert_eq!(preview_fit(300., (1200., 750.)), (187.5, 0., 0.25));
        // A tall page is cut at the bottom, at the preview's tallest.
        assert_eq!(preview_fit(300., (600., 1200.)), (220., 0., 0.5));
        // A very wide one keeps the shortest height, cut at both sides.
        let (height, left, scale) = preview_fit(300., (1400., 280.));
        assert_eq!(height, 140.);
        assert_eq!(scale, 0.5);
        assert_eq!(left, -200.);
        // No picture yet: the shortest, with nothing to place.
        assert_eq!(preview_fit(300., (0., 0.)), (140., 0., 0.));
    }

    #[test]
    fn a_page_is_named_by_its_site() {
        assert_eq!(site("https://example.com/docs?page=2"), "example.com");
        assert_eq!(site("http://127.0.0.1:18090/index.html"), "127.0.0.1:18090");
        assert_eq!(site("example.org#top"), "example.org");
        assert_eq!(site("file:///tmp/a.html"), "");
    }

    #[test]
    fn a_picture_comes_out_blue_before_red() {
        let mut red = image::RgbImage::new(2, 2);
        for pixel in red.pixels_mut() {
            pixel.0 = [250, 0, 0];
        }
        let mut jpeg = Vec::new();
        image::DynamicImage::ImageRgb8(red.clone())
            .write_to(
                &mut std::io::Cursor::new(&mut jpeg),
                image::ImageFormat::Jpeg,
            )
            .unwrap();
        let frame = picture(&jpeg).unwrap();
        let bytes = frame.as_bytes(0).unwrap();
        assert!(bytes[2] > 200 && bytes[0] < 60, "{:?}", &bytes[..4]);
        // A still comes as a PNG, the same way round.
        let mut png = Vec::new();
        image::DynamicImage::ImageRgb8(red)
            .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
            .unwrap();
        let still = picture(&png).unwrap();
        assert_eq!(&still.as_bytes(0).unwrap()[..3], &[0, 0, 250]);
        assert!(picture(b"not a picture").is_none());
    }
}
