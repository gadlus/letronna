//! Watching the browser: pictures of the page, where the agent is about to
//! act, and which page is open, for the window to show beside the chat --
//! and what the user does there, sent on to the page.
//!
//! The browser draws the page into pictures anyway; screencasting hands them
//! over as JPEGs, and only when something on the page changed, so a page
//! standing still costs nothing. They come over a second connection to the
//! tab, read by a thread that runs only while the window watches, and at
//! most one picture every `FRAME_GAP` is handed on -- always the newest, so
//! the last one shown is never out of date.
//!
//! Pictures and everything else travel apart. A window behind with its
//! pictures may skip some, as only the newest matters; a pointer or a new
//! page must never be skipped, and there are few of them.
//!
//! A browser with no window of its own lays the page out in the room the
//! window has for it, at the screen's own sharpness, as a browser that size
//! would. The user's clicks, keys and scrolling go back over the same
//! connection without waiting for answers, looked for every `INPUT_GAP`.

use crate::browser::{follow_opened_tab, follow_remaining_tab, key, key_events};
use crate::cdp::{Event, Ignored, Tab};
use serde_json::{Value, json};
use std::collections::VecDeque;
use std::sync::{Condvar, Mutex, OnceLock};
use std::time::{Duration, Instant};

/// How the pointer's time to arrive and the click it makes are shared out:
/// the window moves it for this long, and an action being watched waits
/// this long before it happens, so the two land together.
pub const POINTER_TIME: Duration = Duration::from_millis(420);
/// At most one picture this often, thirty a second: scrolling reads as
/// smooth, and a busy page cannot keep the window drawing faster than that.
const FRAME_GAP: Duration = Duration::from_millis(33);
/// How often the watcher looks for what the user did: once a frame of the
/// screen, so a click or a key goes on without a lag anyone could see.
const INPUT_GAP: Duration = Duration::from_millis(16);
/// The largest picture asked for before the window says how much room it
/// has, in pixels.
const FRAME_SIZE: u32 = 960;
/// How sharp the pictures are, out of 100: text stays crisp, at about half
/// the size of 90.
const QUALITY: u32 = 80;
/// At most one new layout this often while the window is being resized, so
/// dragging its edge does not have the page laid out at every step.
const FIT_GAP: Duration = Duration::from_millis(100);
/// The smallest room a page is laid out in: less is a window mid-layout.
const LEAST_ROOM: f64 = 120.;
/// How long a page stays still before a sharp picture of it is taken:
/// longer than the gap between a scroll's pictures, so a scroll takes none.
const STILL: Duration = Duration::from_millis(250);
/// At most one sharp picture this often, so a page with a blinking caret or
/// a slow animation cannot keep the browser drawing them.
const STILL_GAP: Duration = Duration::from_secs(1);
/// How long a question to the page may take before it is let go of: one that
/// runs in the page waits while a dialog is open.
const QUICK: Duration = Duration::from_secs(2);
/// What the user did that has not gone to the page yet, at most. More is a
/// page that stopped answering, and the oldest is dropped.
const MAX_CONTROLS: usize = 256;
/// How long the watcher waits for something to happen before it checks
/// that the window is still watching, when it has no tab to watch.
const LOOK_AGAIN: Duration = Duration::from_millis(500);
/// How often, while pictures come, the watcher asks which page they are of:
/// a page changes its own title without saying so.
const NAME_AGAIN: Duration = Duration::from_secs(1);
/// What is selected on the page, as copying takes it: inside a text field,
/// the field's own selection, which the page's selection does not include;
/// nothing from a password field, as a browser copies nothing from one.
const SELECTED: &str = "(() => {
  const field = document.activeElement;
  if (field && typeof field.selectionStart === 'number' && typeof field.value === 'string')
    return field.type === 'password' ? '' : field.value.slice(field.selectionStart, field.selectionEnd);
  return String(getSelection());
})()";

/// The page as it looks now, and how much of it that shows in CSS pixels,
/// which pointers are given in.
pub struct Frame {
    /// A JPEG while the page moves, and a PNG once it is still.
    pub image: Vec<u8>,
    pub width: f64,
    pub height: f64,
}

/// Anything else the window is shown.
#[derive(Debug)]
pub enum Sight {
    /// Where the agent is about to act, the box of what it acts on, and
    /// what it is doing there: "Clicking button \"Sign in\"".
    Pointer {
        x: f64,
        y: f64,
        area: [f64; 4],
        doing: String,
    },
    /// The page now open.
    Page { title: String, url: String },
    /// Whether the tab has a page to go back to, and one to go forward to.
    History { back: bool, forward: bool },
    /// Whether the page is loading.
    Loading(bool),
    /// A question the page asks -- "Leave this page?" -- for the user to
    /// answer, until it has been answered, by anyone.
    Dialog(Option<String>),
    /// What the user copied from the page.
    Copied(String),
    /// The tab or the whole browser was closed; the agent opens it again
    /// when it next uses it.
    Closed,
}

/// What the window receives while it watches. Dropping it stops the
/// watching.
pub struct Watching {
    pub frames: async_channel::Receiver<Frame>,
    pub sights: async_channel::Receiver<Sight>,
}

/// The room the window has for the page.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Room {
    /// In CSS pixels.
    pub width: f64,
    pub height: f64,
    /// The screen's pixels to a CSS pixel.
    pub scale: f64,
    /// Lay the page out in the room. Otherwise it keeps the browser's own
    /// size, and is shown smaller.
    pub fit: bool,
    pub layout: Layout,
    /// How large the page's text and pictures are drawn: 1.25 a quarter
    /// larger, with its text wrapping to fit.
    pub zoom: f64,
}

/// How a page is laid out in the room.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub enum Layout {
    /// As a phone lays pages out: one made for a screen fits the room's
    /// width, and one made wider is shown whole, smaller.
    #[default]
    Portrait,
    /// As a desktop browser's window does, filling the room: one made wider
    /// runs off its side, at its own size.
    Landscape,
}

/// Modifier keys held down.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Held {
    pub alt: bool,
    pub control: bool,
    /// Command on a Mac, the Windows key elsewhere.
    pub meta: bool,
    pub shift: bool,
}

impl Held {
    /// As the protocol counts them.
    fn mask(self) -> i64 {
        i64::from(self.alt)
            | i64::from(self.control) << 1
            | i64::from(self.meta) << 2
            | i64::from(self.shift) << 3
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Button {
    None,
    Left,
    Middle,
    Right,
}

impl Button {
    fn name(self) -> &'static str {
        match self {
            Button::None => "none",
            Button::Left => "left",
            Button::Middle => "middle",
            Button::Right => "right",
        }
    }

    /// As the protocol counts buttons held down.
    fn mask(self) -> i64 {
        match self {
            Button::None => 0,
            Button::Left => 1,
            Button::Right => 2,
            Button::Middle => 4,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Mouse {
    Down,
    Up,
    /// Moved, with `button` held down if one is.
    Moved,
}

/// What the user does to the page in the window. Points are in the page's
/// CSS pixels.
#[derive(Debug, PartialEq)]
pub enum Control {
    Mouse {
        event: Mouse,
        x: f64,
        y: f64,
        button: Button,
        clicks: usize,
        held: Held,
    },
    Wheel {
        x: f64,
        y: f64,
        dx: f64,
        dy: f64,
        held: Held,
    },
    /// A key, by the window's name for it -- "enter", "a" -- and what it
    /// types, if anything: "A" with Shift.
    Key {
        name: String,
        text: Option<String>,
        held: Held,
    },
    /// Text put in as a paste does.
    Paste(String),
    /// Copy what is selected on the page, which comes back as
    /// `Sight::Copied`.
    Copy,
    Back,
    Forward,
    Reload,
    /// Stop the page loading.
    Stop,
    /// Answer the page's question: OK, or Cancel.
    Answer(bool),
}

/// The tab being watched.
#[derive(Clone, PartialEq)]
struct Followed {
    /// Its debugging socket.
    socket: String,
    id: String,
    /// In a browser with no window of its own.
    headless: bool,
}

#[derive(Default)]
struct Watch {
    /// Where pictures go, and everything else, while the window watches.
    to: Option<(async_channel::Sender<Frame>, async_channel::Sender<Sight>)>,
    /// The tab the agent is using.
    tab: Option<Followed>,
    /// Whether the thread reading pictures is running.
    running: bool,
    /// The room the window last said it has.
    room: Option<Room>,
    /// The scale of the window's screen, which a browser started from now on
    /// draws at.
    scale: Option<f64>,
    /// What the user did that has not gone to the page yet, oldest first.
    controls: VecDeque<Control>,
}

impl Watch {
    fn watched(&self) -> bool {
        self.to
            .as_ref()
            .is_some_and(|(frames, sights)| !frames.is_closed() && !sights.is_closed())
    }
}

fn state() -> &'static (Mutex<Watch>, Condvar) {
    static WATCH: OnceLock<(Mutex<Watch>, Condvar)> = OnceLock::new();
    WATCH.get_or_init(Default::default)
}

/// Start watching the browser.
pub fn watch() -> Watching {
    let (frames_to, frames) = async_channel::bounded(2);
    let (sights_to, sights) = async_channel::unbounded();
    let (lock, wake) = state();
    if let Ok(mut watch) = lock.lock() {
        watch.to = Some((frames_to, sights_to));
        if !watch.running {
            watch.running = true;
            std::thread::spawn(run);
        }
        wake.notify_all();
    }
    Watching { frames, sights }
}

/// Say how much room the window has for the page.
pub fn fit(room: Room) {
    if let Ok(mut watch) = state().0.lock() {
        watch.room = Some(room);
        watch.scale = Some(room.scale);
    }
}

/// Say the scale of the window's screen -- 1.5 on a laptop, 2 on a Mac --
/// before the browser starts. A browser with no window draws at it, so its
/// pictures are as sharp as the screen: it cannot be changed once running.
pub fn screen(scale: f64) {
    if let Ok(mut watch) = state().0.lock() {
        watch.scale = Some(scale);
    }
}

/// The scale a browser starting now draws at, if the window has said.
pub(crate) fn scale() -> Option<f64> {
    state().0.lock().ok().and_then(|watch| watch.scale)
}

/// Do something to the page, as the user did in the window. Nothing
/// happens while no page is watched.
pub fn control(control: Control) {
    if let Ok(mut watch) = state().0.lock()
        && watch.watched()
        && watch.tab.is_some()
    {
        if watch.controls.len() >= MAX_CONTROLS {
            watch.controls.pop_front();
        }
        watch.controls.push_back(control);
    }
}

/// Whether the window is watching, so an action should show where it acts.
pub(crate) fn watched() -> bool {
    state().0.lock().is_ok_and(|watch| watch.watched())
}

/// The tab the agent now uses, which the pictures follow.
pub(crate) fn follow(socket: &str, id: &str, headless: bool) {
    let (lock, wake) = state();
    if let Ok(mut watch) = lock.lock() {
        watch.tab = Some(Followed {
            socket: socket.to_owned(),
            id: id.to_owned(),
            headless,
        });
        wake.notify_all();
    }
}

/// Nothing for the window to follow: the conversation it shows has no
/// browser. It is told, so it lets go of the last picture.
pub(crate) fn unfollow() {
    let (lock, wake) = state();
    if let Ok(mut watch) = lock.lock() {
        if watch.tab.take().is_some()
            && let Some((_, sights)) = watch.to.as_ref()
        {
            let _ = sights.try_send(Sight::Closed);
        }
        wake.notify_all();
    }
}

/// Show the window something, if it is watching.
pub(crate) fn show(sight: Sight) {
    if let Ok(watch) = state().0.lock()
        && let Some((_, sights)) = watch.to.as_ref()
    {
        let _ = sights.try_send(sight);
    }
}

/// Hand the window a picture. One still waiting to be drawn is enough: a
/// window behind skips this one and gets a newer.
fn picture(frame: Frame) {
    if let Ok(watch) = state().0.lock()
        && let Some((frames, _)) = watch.to.as_ref()
    {
        let _ = frames.try_send(frame);
    }
}

/// What the user did since the watcher last looked.
fn take_controls() -> Vec<Control> {
    state()
        .0
        .lock()
        .map(|mut watch| coalesced(watch.controls.drain(..)))
        .unwrap_or_default()
}

/// A run of mouse moves as only the last of them: where the mouse is now is
/// what the page needs, and a moving mouse sends one every few pixels.
fn coalesced(controls: impl IntoIterator<Item = Control>) -> Vec<Control> {
    let moved = |control: &Control| {
        matches!(
            control,
            Control::Mouse {
                event: Mouse::Moved,
                ..
            }
        )
    };
    let mut out: Vec<Control> = Vec::new();
    for control in controls {
        if moved(&control) && out.last().is_some_and(moved) {
            out.pop();
        }
        out.push(control);
    }
    out
}

/// The watcher: pictures from whichever tab is in use, for as long as the
/// window watches.
fn run() {
    let (lock, wake) = state();
    loop {
        let tab = {
            let Ok(mut watch) = lock.lock() else {
                return;
            };
            if !watch.watched() {
                watch.to = None;
                watch.running = false;
                watch.controls.clear();
                return;
            }
            match watch.tab.clone() {
                Some(tab) => tab,
                // Nothing to watch yet: wait for a tab, or to stop.
                None => {
                    let _ = wake.wait_timeout(watch, LOOK_AGAIN);
                    continue;
                }
            }
        };
        if stream(&tab).is_ok() {
            continue;
        }
        // Gone. The browser may have another tab to carry on in, as when a
        // sign-in window closes itself; asking takes the browser from the
        // agent, which may wait for its action to end, so not on this thread.
        std::thread::spawn(follow_remaining_tab);
        let Ok(mut watch) = lock.lock() else {
            return;
        };
        // Nothing new to follow yet: say so once, and wait for a tab rather
        // than knock on this one.
        if watch.tab.as_ref() == Some(&tab) {
            watch.tab = None;
            if let Some((_, sights)) = watch.to.as_ref() {
                let _ = sights.try_send(Sight::Closed);
            }
        }
        let _ = wake.wait_timeout(watch, LOOK_AGAIN);
    }
}

/// Whether the window still wants pictures from this tab.
fn still(socket: &str) -> bool {
    state().0.lock().is_ok_and(|watch| {
        watch.watched() && watch.tab.as_ref().is_some_and(|tab| tab.socket == socket)
    })
}

/// Hand on pictures from one tab, and what the user does to it, until the
/// window stops watching it.
fn stream(followed: &Followed) -> Result<(), String> {
    let mut stream = Stream::start(followed)?;
    while still(&followed.socket) {
        stream.turn()?;
    }
    // The page open now keeps the size it was zoomed to otherwise.
    let _ = stream.zoom_to(1.);
    let _ = stream.tab.call::<Ignored>("Page.stopScreencast", json!({}));
    Ok(())
}

fn hash(bytes: &[u8]) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    bytes.hash(&mut hasher);
    hasher.finish()
}

/// Script that zooms a page once its root is there, which a new page's is
/// not yet: `''` sets it back.
fn zoom_script(zoom: &str) -> String {
    format!(
        "(() => {{ const set = () => document.documentElement && \
         (document.documentElement.style.zoom = '{zoom}', true); if (!set()) new \
         MutationObserver((_, watcher) => set() && watcher.disconnect()).observe(document, \
         {{childList: true}}); }})()"
    )
}

/// One tab, watched.
struct Stream {
    tab: Tab,
    headless: bool,
    /// The room the page was last fitted to, and when.
    room: Option<Room>,
    fitted: Instant,
    /// Whether the page is laid out to the room now.
    laid_out: bool,
    /// The newest picture not handed on yet, with the name its
    /// acknowledgement gives it, and when one last was handed on.
    held: Option<(Frame, Value)>,
    shown: Instant,
    /// The page the window was last told of, and when that was asked.
    named: Option<Named>,
    asked: Instant,
    /// The page moved on, so it is asked for its name again.
    moved: bool,
    /// What the page's open question offers as its answer, for a prompt.
    offered: String,
    /// How much of the page its pictures show, in CSS pixels, as the last
    /// one said; when that came, and whether a sharp one was taken since,
    /// and when.
    size: Option<(f64, f64)>,
    pictured: Instant,
    sharp: bool,
    sharpened: Instant,
    /// The last picture the screencast sent, as a hash: one the same, as a
    /// still page sends when a sharp picture is taken of it, changes
    /// nothing.
    last_seen: u64,
    /// The zoom the page is drawn at, and the script keeping it on the
    /// pages opened after.
    zoom: f64,
    zoom_script: Option<String>,
}

impl Stream {
    fn start(followed: &Followed) -> Result<Stream, String> {
        let mut tab = Tab::connect(&followed.socket, &followed.id)?;
        // Dialogs, loads and new tabs are heard here too, and a dialog
        // waits for the user.
        tab.leaves_dialogs = true;
        tab.call::<Ignored>("Page.enable", json!({}))?;
        // Done to a page before this one, or to another tab.
        drop(take_controls());
        let mut stream = Stream {
            tab,
            headless: followed.headless,
            room: None,
            fitted: Instant::now() - FIT_GAP,
            laid_out: false,
            held: None,
            shown: Instant::now() - FRAME_GAP,
            named: None,
            asked: Instant::now(),
            moved: false,
            offered: String::new(),
            size: None,
            pictured: Instant::now(),
            sharp: true,
            sharpened: Instant::now() - STILL_GAP,
            last_seen: 0,
            zoom: 1.,
            zoom_script: None,
        };
        if !stream.refit()? {
            stream.cast()?;
        }
        stream.named = name_page(&mut stream.tab, None);
        Ok(stream)
    }

    /// Wait a moment for what the page says, then pass on what the user
    /// did and what the page shows.
    fn turn(&mut self) -> Result<(), String> {
        let wait = match self.held {
            Some(_) => FRAME_GAP.saturating_sub(self.shown.elapsed()),
            None => INPUT_GAP,
        };
        self.tab
            .wait(wait.clamp(Duration::from_millis(1), INPUT_GAP), |e| {
                e.method == "Page.screencastFrame"
            })?;
        let events: Vec<Event> = self.tab.events.drain(..).collect();
        for event in events {
            self.hear(event)?;
        }
        self.refit()?;
        for control in take_controls() {
            // A page too busy to answer a question is not a closed one.
            if let Err(error) = self.obey(control)
                && self.tab.broken
            {
                return Err(error);
            }
        }
        if self.shown.elapsed() >= FRAME_GAP
            && let Some((frame, named)) = self.held.take()
        {
            self.acknowledge(named)?;
            picture(frame);
            self.shown = Instant::now();
            self.moved |= self.asked.elapsed() >= NAME_AGAIN;
        }
        if !self.sharp
            && self.held.is_none()
            && self.pictured.elapsed() >= STILL
            && self.sharpened.elapsed() >= STILL_GAP
        {
            self.sharpen();
        }
        if self.moved {
            self.named = name_page(&mut self.tab, self.named.take());
            self.asked = Instant::now();
            self.moved = false;
        }
        Ok(())
    }

    fn hear(&mut self, event: Event) -> Result<(), String> {
        let params = &event.params;
        let here = params["frameId"] == self.tab.id.as_str();
        match event.method.as_str() {
            "Page.screencastFrame" => {
                let size = &params["metadata"];
                // A page made wider than the room is shown whole, smaller.
                let zoom = size["pageScaleFactor"]
                    .as_f64()
                    .filter(|zoom| *zoom > 0.)
                    .unwrap_or(1.);
                let named = params["sessionId"].clone();
                let Some(jpeg) = params["data"].as_str().and_then(crate::media::base64) else {
                    return self.acknowledge(named);
                };
                // The page as it was: nothing to draw again, and nothing moved.
                let seen = hash(&jpeg);
                if seen == self.last_seen {
                    return self.acknowledge(named);
                }
                self.last_seen = seen;
                let frame = Frame {
                    image: jpeg,
                    width: size["deviceWidth"].as_f64().unwrap_or(0.) / zoom,
                    height: size["deviceHeight"].as_f64().unwrap_or(0.) / zoom,
                };
                self.size = Some((frame.width, frame.height));
                self.pictured = Instant::now();
                self.sharp = false;
                // A browser sends the next only once this one is
                // acknowledged, so one that sent more is told of the one
                // passed over.
                if let Some((_, passed)) = self.held.replace((frame, named)) {
                    self.acknowledge(passed)?;
                }
            }
            "Page.javascriptDialogOpening" => {
                self.offered = params["defaultPrompt"]
                    .as_str()
                    .unwrap_or_default()
                    .to_owned();
                let asked = params["message"].as_str().unwrap_or_default();
                show(Sight::Dialog(Some(asked.to_owned())));
            }
            "Page.javascriptDialogClosed" => show(Sight::Dialog(None)),
            "Page.frameStartedLoading" if here => show(Sight::Loading(true)),
            "Page.frameStoppedLoading" if here => {
                show(Sight::Loading(false));
                self.moved = true;
            }
            "Page.frameNavigated" if params["frame"]["id"] == self.tab.id.as_str() => {
                self.moved = true;
            }
            "Page.navigatedWithinDocument" if here => self.moved = true,
            // Opened by the user's click here; the agent follows the tabs
            // its own actions open. Taking the browser from the agent may
            // wait for its action to end, so not on this thread.
            "Page.windowOpen" => {
                std::thread::spawn(follow_opened_tab);
            }
            _ => {}
        }
        Ok(())
    }

    /// Tell the browser a picture was taken, so it draws the next. Only
    /// once it is handed on: the browser then draws no more pictures than
    /// the window shows, where it would otherwise draw one for every change
    /// on the page, twice as many while it scrolls.
    fn acknowledge(&mut self, named: Value) -> Result<(), String> {
        self.tab
            .tell("Page.screencastFrameAck", json!({"sessionId": named}))
    }

    /// Lay the page out in the room the window now has, if that changed.
    /// Whether there is a room to lay it out in.
    fn refit(&mut self) -> Result<bool, String> {
        let room = state().0.lock().ok().and_then(|watch| watch.room);
        let Some(room) = room.filter(|room| room.width >= LEAST_ROOM && room.height >= LEAST_ROOM)
        else {
            return Ok(false);
        };
        if self.room == Some(room) || self.fitted.elapsed() < FIT_GAP {
            return Ok(true);
        }
        self.room = Some(room);
        self.fitted = Instant::now();
        // A window of its own is the user's to size, and the page fits it.
        // The browser's own scale stays: the pictures are drawn at that.
        // Portrait, laid out as a phone lays pages out: one made to fit a
        // screen fits the room, and one made wider is shown whole, smaller,
        // rather than cut off at the room's edge. Landscape, as a desktop
        // browser's window of the room's size lays it out.
        let lay_out = room.fit && self.headless;
        if lay_out {
            let phone = room.layout == Layout::Portrait;
            self.tab.call::<Ignored>(
                "Emulation.setDeviceMetricsOverride",
                json!({"width": room.width.round() as i64, "height": room.height.round() as i64,
                       "deviceScaleFactor": 0, "mobile": phone}),
            )?;
        } else if self.laid_out {
            self.tab
                .call::<Ignored>("Emulation.clearDeviceMetricsOverride", json!({}))?;
        }
        self.laid_out = lay_out;
        // A window of its own has its own zoom.
        self.zoom_to(if lay_out { room.zoom } else { 1. })?;
        let _ = self.tab.call::<Ignored>("Page.stopScreencast", json!({}));
        self.cast()?;
        Ok(true)
    }

    /// Draw the page's text and pictures `zoom` times as large, on this page
    /// and the ones opened after, while the window watches. CSS zoom: the
    /// page reflows, and is drawn at the screen's own sharpness, where
    /// stretching its pictures would blur them.
    fn zoom_to(&mut self, zoom: f64) -> Result<(), String> {
        if (zoom - self.zoom).abs() < 0.001 {
            return Ok(());
        }
        if let Some(script) = self.zoom_script.take() {
            let _ = self.tab.call::<Ignored>(
                "Page.removeScriptToEvaluateOnNewDocument",
                json!({"identifier": script}),
            );
        }
        let size = match (zoom - 1.).abs() < 0.001 {
            true => String::new(),
            false => format!("{zoom}"),
        };
        if !size.is_empty() {
            let added: Value = self.tab.call(
                "Page.addScriptToEvaluateOnNewDocument",
                json!({"source": zoom_script(&size)}),
            )?;
            self.zoom_script = added["identifier"].as_str().map(str::to_owned);
        }
        let _ = self.tab.call_within::<Ignored>(
            "Runtime.evaluate",
            json!({"expression": zoom_script(&size)}),
            QUICK,
        );
        self.zoom = zoom;
        Ok(())
    }

    /// A sharp picture of the page as it stands, in place of the last the
    /// screencast sent: a JPEG blurs small text and fine lines, where a PNG
    /// keeps them. Taken once the page is still, as it is costlier to make,
    /// and only of a page laid out in the room, which it pictures pixel for
    /// pixel, as the screencast does: one larger is shown shrunk, which
    /// hides the blur anyway.
    fn sharpen(&mut self) {
        self.sharp = true;
        self.sharpened = Instant::now();
        let (Some((width, height)), true) = (self.size, self.laid_out) else {
            return;
        };
        let shot = self.tab.call_within::<Value>(
            "Page.captureScreenshot",
            json!({"format": "png"}),
            QUICK,
        );
        let png = shot
            .ok()
            .and_then(|shot| shot["data"].as_str().and_then(crate::media::base64));
        if let Some(png) = png {
            picture(Frame {
                image: png,
                width,
                height,
            });
        }
    }

    /// Ask for pictures as large as the room shows them, at the screen's
    /// own pixels: smaller is blurred, and larger is decoded only to be
    /// shrunk.
    fn cast(&mut self) -> Result<(), String> {
        let (width, height) = match self.room {
            Some(room) => (
                (room.width * room.scale).ceil() as u32,
                (room.height * room.scale).ceil() as u32,
            ),
            None => (FRAME_SIZE, FRAME_SIZE),
        };
        self.tab
            .call::<Ignored>(
                "Page.startScreencast",
                json!({"format": "jpeg", "quality": QUALITY, "maxWidth": width,
                   "maxHeight": height, "everyNthFrame": 1}),
            )
            .map(drop)
    }

    /// Do to the page what the user did. Nothing waits for the page to
    /// answer, except to copy.
    fn obey(&mut self, control: Control) -> Result<(), String> {
        match control {
            Control::Mouse {
                event,
                x,
                y,
                button,
                clicks,
                held,
            } => {
                let (kind, buttons) = match event {
                    Mouse::Down => ("mousePressed", button.mask()),
                    Mouse::Up => ("mouseReleased", 0),
                    Mouse::Moved => ("mouseMoved", button.mask()),
                };
                self.tab.tell(
                    "Input.dispatchMouseEvent",
                    json!({"type": kind, "x": x, "y": y, "button": button.name(),
                           "buttons": buttons, "clickCount": clicks, "modifiers": held.mask()}),
                )
            }
            Control::Wheel { x, y, dx, dy, held } => self.tab.tell(
                "Input.dispatchMouseEvent",
                json!({"type": "mouseWheel", "x": x, "y": y, "deltaX": dx, "deltaY": dy,
                       "modifiers": held.mask()}),
            ),
            Control::Key { name, text, held } => match keystroke(&name, text, held) {
                Keystroke::Press([down, up]) => {
                    self.tab.tell("Input.dispatchKeyEvent", down)?;
                    self.tab.tell("Input.dispatchKeyEvent", up)
                }
                Keystroke::Insert(text) => self.paste(text),
                Keystroke::Nothing => Ok(()),
            },
            Control::Paste(text) => self.paste(text),
            Control::Copy => {
                let copied: Value = self.tab.call_within(
                    "Runtime.evaluate",
                    json!({"expression": SELECTED, "returnByValue": true}),
                    QUICK,
                )?;
                if let Some(text) = copied["result"]["value"].as_str().filter(|t| !t.is_empty()) {
                    show(Sight::Copied(text.to_owned()));
                }
                Ok(())
            }
            Control::Back => self.go(-1),
            Control::Forward => self.go(1),
            Control::Reload => self.tab.tell("Page.reload", json!({})),
            Control::Stop => self.tab.tell("Page.stopLoading", json!({})),
            Control::Answer(agree) => self.tab.tell(
                "Page.handleJavaScriptDialog",
                json!({"accept": agree, "promptText": self.offered}),
            ),
        }
    }

    fn paste(&mut self, text: String) -> Result<(), String> {
        self.tab.tell("Input.insertText", json!({"text": text}))
    }

    /// Go `by` pages back or forward in the tab's history, if it has one
    /// there.
    fn go(&mut self, by: i64) -> Result<(), String> {
        let history: Value = self
            .tab
            .call_within("Page.getNavigationHistory", json!({}), QUICK)?;
        let at = history["currentIndex"].as_i64().unwrap_or(0) + by;
        let entry = usize::try_from(at)
            .ok()
            .and_then(|at| history["entries"].get(at));
        match entry {
            Some(entry) => self.tab.tell(
                "Page.navigateToHistoryEntry",
                json!({"entryId": entry["id"]}),
            ),
            None => Ok(()),
        }
    }
}

/// What pressing a key in the window does to the page.
#[derive(Debug, PartialEq)]
enum Keystroke {
    /// Its going down and coming up again.
    Press([Value; 2]),
    /// Text from an input method, more than one key could type.
    Insert(String),
    /// A key the page is not told of, such as F13.
    Nothing,
}

/// A character is typed as what it types -- "A" with Shift -- and any other
/// key is pressed by its name, with the modifiers held.
fn keystroke(name: &str, text: Option<String>, held: Held) -> Keystroke {
    // AltGr is Ctrl and Alt together, and types.
    let shortcut = (held.control || held.alt || held.meta) && !(held.control && held.alt);
    let typed = text.filter(|text| !shortcut && !text.is_empty() && name.chars().count() == 1);
    let found = key(typed.as_deref().unwrap_or(name));
    // Typing, only Shift counts: the rest made the character.
    let shift = Held {
        shift: held.shift,
        ..Held::default()
    };
    match (found, typed) {
        (Some(found), Some(_)) => Keystroke::Press(key_events(&found, shift.mask())),
        (Some(found), None) => {
            let [mut down, up] = key_events(&found, held.mask());
            if let Some(command) = command(name, held) {
                down["commands"] = json!([command]);
            }
            Keystroke::Press([down, up])
        }
        (None, Some(text)) => Keystroke::Insert(text),
        (None, None) => Keystroke::Nothing,
    }
}

/// The editing command a shortcut runs on a Mac, where the browser's window
/// runs it rather than the page, so one pressed from here must name it.
fn command(name: &str, held: Held) -> Option<&'static str> {
    if !cfg!(target_os = "macos") || !held.meta {
        return None;
    }
    match (name.to_ascii_lowercase().as_str(), held.shift) {
        ("a", _) => Some("selectAll"),
        ("z", false) => Some("undo"),
        ("z", true) => Some("redo"),
        _ => None,
    }
}

/// The page a tab is on, and where its history goes from there.
#[derive(PartialEq)]
struct Named {
    title: String,
    url: String,
    back: bool,
    forward: bool,
}

/// Tell the window which page the tab is on, and whether it can go back or
/// forward, when that is not what it was told last. What it was told now.
fn name_page(tab: &mut Tab, told: Option<Named>) -> Option<Named> {
    let history: Value = tab
        .call_within("Page.getNavigationHistory", json!({}), QUICK)
        .ok()?;
    let at = history["currentIndex"].as_u64().unwrap_or(0) as usize;
    let entries = history["entries"].as_array()?;
    let text = |key: &str| entries.get(at)?[key].as_str().map(str::to_owned);
    // The blank page a tab opens on is nowhere to go back to.
    let somewhere = |entry: &Value| entry["url"] != "about:blank";
    let now = Named {
        title: text("title").unwrap_or_default(),
        url: text("url").unwrap_or_default(),
        back: entries.iter().take(at).any(somewhere),
        forward: at + 1 < entries.len(),
    };
    let told = told.as_ref();
    if told.is_none_or(|told| (&told.title, &told.url) != (&now.title, &now.url)) {
        show(Sight::Page {
            title: now.title.clone(),
            url: now.url.clone(),
        });
    }
    if told.is_none_or(|told| (told.back, told.forward) != (now.back, now.forward)) {
        show(Sight::History {
            back: now.back,
            forward: now.forward,
        });
    }
    Some(now)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;
    use tungstenite::Message;

    #[test]
    fn a_run_of_mouse_moves_goes_on_as_the_last() {
        let moved = |x: f64| Control::Mouse {
            event: Mouse::Moved,
            x,
            y: 0.,
            button: Button::None,
            clicks: 0,
            held: Held::default(),
        };
        let down = Control::Mouse {
            event: Mouse::Down,
            x: 3.,
            y: 0.,
            button: Button::Left,
            clicks: 1,
            held: Held::default(),
        };
        let sent = coalesced([moved(1.), moved(2.), moved(3.), down, moved(4.), moved(5.)]);
        assert_eq!(sent.len(), 3, "{sent:?}");
        assert_eq!(sent[0], moved(3.));
        assert_eq!(sent[2], moved(5.));
    }

    #[test]
    fn keys_reach_the_page_as_a_keyboard_sends_them() {
        let shift = Held {
            shift: true,
            ..Held::default()
        };
        let control = Held {
            control: true,
            ..Held::default()
        };
        let pressed = |name: &str, text: Option<&str>, held: Held| match keystroke(
            name,
            text.map(str::to_owned),
            held,
        ) {
            Keystroke::Press([down, up]) => (down, up),
            other => panic!("{name}: {other:?}"),
        };
        // Shift and a letter type the capital.
        let (down, up) = pressed("a", Some("A"), shift);
        assert_eq!(
            (
                &down["type"],
                &down["key"],
                &down["text"],
                &down["modifiers"]
            ),
            (&json!("keyDown"), &json!("A"), &json!("A"), &json!(8))
        );
        assert_eq!((&up["type"], &up["key"]), (&json!("keyUp"), &json!("A")));
        // A shortcut presses the key and types nothing.
        let (down, _) = pressed("a", None, control);
        assert_eq!(
            (&down["type"], &down["text"], &down["modifiers"]),
            (&json!("rawKeyDown"), &Value::Null, &json!(2))
        );
        // Keys the window names its own way.
        assert_eq!(pressed("enter", None, Held::default()).0["text"], "\r");
        assert_eq!(pressed("up", None, Held::default()).0["key"], "ArrowUp");
        assert_eq!(pressed("space", Some(" "), Held::default()).0["text"], " ");
        // AltGr types what the layout puts there.
        let altgr = Held {
            control: true,
            alt: true,
            ..Held::default()
        };
        assert_eq!(pressed("e", Some("€"), altgr).0["text"], "€");
        assert_eq!(keystroke("f13", None, Held::default()), Keystroke::Nothing);
    }

    /// A fake tab: answers what the watcher asks as a browser would, puts
    /// up a dialog when it is clicked, and tells the test what it heard.
    fn fake_tab(listener: TcpListener, heard: std::sync::mpsc::Sender<Value>) {
        let (stream, _) = listener.accept().unwrap();
        let mut browser = tungstenite::accept(stream).unwrap();
        let send = |browser: &mut tungstenite::WebSocket<std::net::TcpStream>, value: Value| {
            browser.send(Message::text(value.to_string())).unwrap();
        };
        while let Ok(Message::Text(text)) = browser.read() {
            let request: Value = serde_json::from_str(text.as_str()).unwrap();
            let method = request["method"].as_str().unwrap().to_owned();
            let result = match method.as_str() {
                "Page.getNavigationHistory" => json!({"currentIndex": 1, "entries": [
                    {"id": 6, "title": "Before", "url": "http://fake/before"},
                    {"id": 7, "title": "Fake page", "url": "http://fake/"}]}),
                "Runtime.evaluate" => json!({"result": {"type": "string", "value": "picked"}}),
                "Page.getLayoutMetrics" => json!({"cssVisualViewport":
                    {"pageX": 0, "pageY": 0, "clientWidth": 1280, "clientHeight": 800}}),
                // A PNG's first eight bytes.
                "Page.captureScreenshot" => json!({"data": "iVBORw0KGgo="}),
                "Page.addScriptToEvaluateOnNewDocument" => json!({"identifier": "7"}),
                _ => json!({}),
            };
            send(&mut browser, json!({"id": request["id"], "result": result}));
            if method == "Page.startScreencast" {
                // Three pictures at once, as scrolling makes them.
                for (n, data) in ["b25l", "dHdv", "dGhyZWU="].iter().enumerate() {
                    let frame = json!({"method": "Page.screencastFrame", "params": {
                        "data": data, "sessionId": n,
                        "metadata": {"deviceWidth": 640, "deviceHeight": 400,
                                     "pageScaleFactor": 0.5}}});
                    send(&mut browser, frame);
                }
            }
            if method == "Input.dispatchMouseEvent" && request["params"]["type"] == "mousePressed" {
                send(
                    &mut browser,
                    json!({"method": "Page.javascriptDialogOpening",
                           "params": {"type": "confirm", "message": "Sure?", "defaultPrompt": ""}}),
                );
            }
            // Ends when the watcher lets go of the tab, and the test may
            // have stopped listening by then.
            let _ = heard.send(request);
        }
    }

    #[test]
    fn the_window_sees_the_newest_picture_and_the_page_does_what_the_user_does() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let socket = format!("ws://{}/devtools/page/W1", listener.local_addr().unwrap());
        let (heard_to, heard) = std::sync::mpsc::channel();
        std::thread::spawn(move || fake_tab(listener, heard_to));
        let wait_for = |method: &str| -> Value {
            loop {
                let request: Value = heard.recv_timeout(Duration::from_secs(5)).unwrap();
                if request["method"] == method {
                    return request["params"].clone();
                }
            }
        };
        let watching = watch();
        follow(&socket, "W1", true);
        let seen = |frame: Frame| (String::from_utf8(frame.image).unwrap(), frame.width);
        // A burst gives the newest, and nothing after it: never one out of
        // date.
        let newest = watching.frames.recv_blocking().unwrap();
        assert_eq!(seen(newest), ("three".to_owned(), 1280.));
        std::thread::sleep(FRAME_GAP * 3);
        assert!(watching.frames.try_recv().is_err());
        // The page is named as the watching starts.
        let named = watching.sights.recv_blocking().unwrap();
        assert!(matches!(named, Sight::Page { title, .. } if title == "Fake page"));

        // The room the window has: the page is laid out in it, at the
        // screen's sharpness, and pictures come that size.
        fit(Room {
            width: 500.,
            height: 700.,
            scale: 1.5,
            fit: true,
            layout: Layout::Portrait,
            zoom: 1.25,
        });
        let laid_out = wait_for("Emulation.setDeviceMetricsOverride");
        assert_eq!(laid_out["mobile"], true);
        // Zoomed on this page and the ones after.
        let kept = wait_for("Page.addScriptToEvaluateOnNewDocument");
        assert!(
            kept["source"].as_str().unwrap().contains("'1.25'"),
            "{kept}"
        );
        assert_eq!(
            (&laid_out["width"], &laid_out["height"]),
            (&json!(500), &json!(700))
        );
        assert_eq!(scale(), Some(1.5));
        let cast = wait_for("Page.startScreencast");
        assert_eq!(
            (&cast["maxWidth"], &cast["maxHeight"]),
            (&json!(750), &json!(1050))
        );
        // Laid out and still for a moment, the page is pictured again,
        // sharp: a PNG.
        let deadline = Instant::now() + Duration::from_secs(5);
        let sharp = loop {
            assert!(Instant::now() < deadline, "no sharp picture came");
            match watching.frames.try_recv() {
                Ok(frame) if frame.image.starts_with(b"\x89PNG") => break frame,
                _ => std::thread::sleep(Duration::from_millis(10)),
            }
        };
        assert_eq!(sharp.width, 1280.);

        // A click goes to the page as it was made; the page's question
        // waits for the user, and their answer goes back.
        control(Control::Mouse {
            event: Mouse::Down,
            x: 10.,
            y: 20.,
            button: Button::Left,
            clicks: 1,
            held: Held::default(),
        });
        let pressed = wait_for("Input.dispatchMouseEvent");
        assert_eq!(
            (&pressed["type"], &pressed["x"], &pressed["buttons"]),
            (&json!("mousePressed"), &json!(10.), &json!(1))
        );
        let asked = loop {
            match watching.sights.recv_blocking().unwrap() {
                Sight::Dialog(asked) => break asked,
                _ => continue,
            }
        };
        assert_eq!(asked.as_deref(), Some("Sure?"));
        control(Control::Answer(true));
        assert_eq!(wait_for("Page.handleJavaScriptDialog")["accept"], true);

        // Typing, and copying what is selected.
        control(Control::Key {
            name: "a".into(),
            text: Some("a".into()),
            held: Held::default(),
        });
        assert_eq!(wait_for("Input.dispatchKeyEvent")["text"], "a");
        control(Control::Copy);
        let copied = loop {
            match watching.sights.recv_blocking().unwrap() {
                Sight::Copied(text) => break text,
                _ => continue,
            }
        };
        assert_eq!(copied, "picked");
        control(Control::Back);
        assert_eq!(wait_for("Page.navigateToHistoryEntry")["entryId"], 6);

        // Pictures piling up never push out a pointer or a page.
        for n in 0..10 {
            picture(Frame {
                image: vec![n],
                width: 1.,
                height: 1.,
            });
        }
        show(Sight::Page {
            title: "Kept".into(),
            url: "http://x/".into(),
        });
        let kept = loop {
            match watching.sights.try_recv() {
                Ok(Sight::Page { title, .. }) => break title,
                Ok(_) => continue,
                Err(error) => panic!("{error}"),
            }
        };
        assert_eq!(kept, "Kept");
        assert!(watched());
        // Letting go stops the screencast, and the watching.
        drop(watching);
        wait_for("Page.stopScreencast");
        std::thread::sleep(INPUT_GAP * 4);
        assert!(!watched());
    }
}
