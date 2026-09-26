//! Watching the browser: pictures of the page, where the agent is about to
//! act, and which page is open, for the window to show beside the chat.
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

use crate::cdp::{Ignored, Tab};
use serde_json::{Value, json};
use std::sync::{Condvar, Mutex, OnceLock};
use std::time::{Duration, Instant};

/// How the pointer's time to arrive and the click it makes are shared out:
/// the window moves it for this long, and an action being watched waits
/// this long before it happens, so the two land together.
pub const POINTER_TIME: Duration = Duration::from_millis(420);
/// At most one picture this often, about twelve a second: smooth enough for
/// scrolling and typing, and a busy page cannot keep the window drawing.
const FRAME_GAP: Duration = Duration::from_millis(80);
/// The largest picture asked for, in pixels: the docked panel at its widest,
/// on a screen at 150%. A bigger one would be decoded only to be shrunk.
const FRAME_SIZE: u32 = 960;
/// How long the watcher waits for something to happen before it checks
/// that the window is still watching.
const LOOK_AGAIN: Duration = Duration::from_millis(500);

/// The page as it looks now, and its size in CSS pixels, which pointers are
/// given in.
pub struct Frame {
    pub jpeg: Vec<u8>,
    pub width: f64,
    pub height: f64,
}

/// Anything else the window is shown.
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

#[derive(Default)]
struct Watch {
    /// Where pictures go, and everything else, while the window watches.
    to: Option<(async_channel::Sender<Frame>, async_channel::Sender<Sight>)>,
    /// The tab the agent is using: its debugging socket and its id.
    tab: Option<(String, String)>,
    /// Whether the thread reading pictures is running.
    running: bool,
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

/// Whether the window is watching, so an action should show where it acts.
pub(crate) fn watched() -> bool {
    state().0.lock().is_ok_and(|watch| watch.watched())
}

/// The tab the agent now uses, which the pictures follow.
pub(crate) fn follow(socket: &str, id: &str) {
    let (lock, wake) = state();
    if let Ok(mut watch) = lock.lock() {
        watch.tab = Some((socket.to_owned(), id.to_owned()));
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
        if stream(&tab.0, &tab.1).is_ok() {
            continue;
        }
        let Ok(mut watch) = lock.lock() else {
            return;
        };
        // Gone, with nothing new to follow yet: say so once, and wait for
        // the agent to open a tab rather than knock on this one.
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
    state()
        .0
        .lock()
        .is_ok_and(|watch| watch.watched() && watch.tab.as_ref().is_some_and(|tab| tab.0 == socket))
}

/// Hand on pictures from one tab until the window stops watching it.
fn stream(socket: &str, id: &str) -> Result<(), String> {
    let mut tab = Tab::connect(socket, id)?;
    tab.call::<Ignored>(
        "Page.startScreencast",
        json!({"format": "jpeg", "quality": 60, "maxWidth": FRAME_SIZE,
               "maxHeight": FRAME_SIZE, "everyNthFrame": 1}),
    )?;
    let mut shown = Instant::now() - FRAME_GAP;
    let mut held: Option<Frame> = None;
    while still(socket) {
        let wait = match held {
            Some(_) => FRAME_GAP.saturating_sub(shown.elapsed()),
            None => LOOK_AGAIN,
        };
        tab.wait(wait.max(Duration::from_millis(1)), |e| {
            e.method == "Page.screencastFrame"
        })?;
        let frames: Vec<Value> = tab
            .events
            .drain(..)
            .filter(|e| e.method == "Page.screencastFrame")
            .map(|e| e.params)
            .collect();
        for frame in frames {
            // Each is acknowledged, or the browser sends no more.
            tab.call::<Ignored>(
                "Page.screencastFrameAck",
                json!({"sessionId": frame["sessionId"]}),
            )?;
            let size = &frame["metadata"];
            if let Some(jpeg) = frame["data"].as_str().and_then(crate::media::base64) {
                held = Some(Frame {
                    jpeg,
                    width: size["deviceWidth"].as_f64().unwrap_or(0.),
                    height: size["deviceHeight"].as_f64().unwrap_or(0.),
                });
            }
        }
        if shown.elapsed() >= FRAME_GAP
            && let Some(frame) = held.take()
        {
            picture(frame);
            shown = Instant::now();
        }
    }
    let _ = tab.call::<Ignored>("Page.stopScreencast", json!({}));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;
    use tungstenite::Message;

    #[test]
    fn the_window_is_shown_the_newest_picture_and_the_watching_stops_with_it() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let socket = format!("ws://{}/devtools/page/W1", listener.local_addr().unwrap());
        let server = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut browser = tungstenite::accept(stream).unwrap();
            let mut heard = Vec::new();
            while let Ok(Message::Text(text)) = browser.read() {
                let request: Value = serde_json::from_str(text.as_str()).unwrap();
                let method = request["method"].as_str().unwrap().to_owned();
                heard.push(method.clone());
                let answer = json!({"id": request["id"], "result": {}});
                browser.send(Message::text(answer.to_string())).unwrap();
                if method == "Page.startScreencast" {
                    // Three pictures at once, as scrolling makes them.
                    for (n, data) in ["b25l", "dHdv", "dGhyZWU="].iter().enumerate() {
                        let frame = json!({"method": "Page.screencastFrame", "params": {
                            "data": data, "sessionId": n,
                            "metadata": {"deviceWidth": 1280, "deviceHeight": 800}}});
                        browser.send(Message::text(frame.to_string())).unwrap();
                    }
                }
                if method == "Page.stopScreencast" {
                    break;
                }
            }
            heard
        });
        let watching = watch();
        follow(&socket, "W1");
        let seen = |frame: Frame| (String::from_utf8(frame.jpeg).unwrap(), frame.width);
        // The first straight away; then, once the gap has passed, the
        // newest, not the one between.
        let first = watching.frames.recv_blocking().unwrap();
        assert_eq!(seen(first), ("one".to_owned(), 1280.));
        let started = Instant::now();
        let next = watching.frames.recv_blocking().unwrap();
        assert_eq!(seen(next).0, "three");
        assert!(started.elapsed() < FRAME_GAP * 3);
        // Pictures piling up never push out a pointer or a page.
        for n in 0..10 {
            picture(Frame {
                jpeg: vec![n],
                width: 1.,
                height: 1.,
            });
        }
        show(Sight::Page {
            title: "Kept".into(),
            url: "http://x/".into(),
        });
        let kept = watching.sights.try_recv();
        assert!(matches!(kept, Ok(Sight::Page { title, .. }) if title == "Kept"));
        assert!(watched());
        // Letting go stops the screencast.
        drop(watching);
        let heard = server.join().unwrap();
        let acks = heard
            .iter()
            .filter(|m| *m == "Page.screencastFrameAck")
            .count();
        assert_eq!(acks, 3, "{heard:?}");
        assert_eq!(
            heard.last().map(String::as_str),
            Some("Page.stopScreencast")
        );
        assert!(!watched());
    }
}
