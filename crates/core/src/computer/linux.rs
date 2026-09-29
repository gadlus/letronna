//! Computer use on Linux under X11: apps and windows from the X server and
//! the window manager's `_NET_CLIENT_LIST`, pictures through `GetImage`,
//! made-up input through the XTest extension, and elements through AT-SPI
//! over D-Bus where the app shares them.
//!
//! An app is known by its program's name, taken from the process that made
//! its window, which the X server names through the X-Resource extension,
//! so no window can pass itself off as another app's. XTest input goes to
//! whatever is under the pointer or has the keyboard, not to one process,
//! so before each event the app is checked to have the keyboard, and a
//! click or a scroll to land on one of its windows; a button or key that
//! went down is always let up again. An element is pressed, focused or
//! given its text through AT-SPI wherever it takes that, which reaches the
//! element itself whatever is in front.
//!
//! Wayland gives no program the screen or global input without asking
//! through the desktop's portals, so a Wayland session with no X display
//! is refused with that said. Under XWayland, only the apps running on it
//! are seen.
// Tests use a fake backend, so outside the pure parts this is not reached.
#![cfg_attr(test, allow(dead_code))]

mod atspi;
mod x11;

use super::{Act, App, Backend, Bounds, Direction, Element, Modifier, Point, Shot, Spot, lock};
use atspi::{Accessible, Bus};
use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};
use x11::{Keymap, Rgb, Window, X};

/// How long a listing of the apps is used again: the gate and the call it
/// lets through list them moments apart, and a new window is seen soon.
const APPS_FRESH: Duration = Duration::from_millis(500);
/// How long the processes showing this one are used again; a new tmux
/// client is seen within it.
const HOSTS_FRESH: Duration = Duration::from_secs(2);
/// How long one message to an app on the accessibility bus waits, so a
/// hung app stalls a call only briefly.
const APP_ANSWERS: Duration = Duration::from_millis(1500);
/// How long one listing of an app's elements may take before it stops.
const WALK_TIME: Duration = Duration::from_secs(4);
/// The most elements one listing reads: each is several messages to the
/// app, and a web page's full tree would take far longer than the call.
const READ_MOST: usize = 3_000;
/// How deep a listing goes; real windows nest a few dozen levels at most.
const DEPTH_MOST: usize = 60;
/// The most elements one listing returns, over the tool's 400 shown, so it
/// can say how many more there were.
const LISTED_MOST: usize = 1_000;
/// The most of an element's value kept: the tool shows far less, and a text
/// view's value is its whole document.
const TEXT_KEPT: usize = 1_000;
/// The most elements whose keys are kept, over the tool's 10,000 ids.
const KNOWN_KEPT: usize = 20_000;
/// How long an app has to come to the front before nothing is sent to it.
const FRONT_WAIT: Duration = Duration::from_secs(1);
/// How often the window with the keyboard is looked at while waiting.
const FRONT_POLL: Duration = Duration::from_millis(50);
/// The pause after each made-up event, which apps need to take them in turn.
const EVENT_GAP: Duration = Duration::from_millis(20);
/// The pause before a key lent to a character is given back, so the app
/// has read the key press under the mapping it was typed with.
const LEND_GAP: Duration = Duration::from_millis(50);
/// The furthest up the parents a walk goes, far past any real chain.
const ANCESTORS_MOST: usize = 64;
/// Terminal multiplexers, whose server has no terminal of its own: the
/// terminals showing Letronna are those of its clients.
const MULTIPLEXERS: &[&str] = &["tmux", "screen", "zellij"];

/// Roles listed though they have no name or value: what can be pressed,
/// typed into or moved.
const ACTIONABLE: &[&str] = &[
    "push button",
    "toggle button",
    "check box",
    "radio button",
    "combo box",
    "entry",
    "password text",
    "spin button",
    "slider",
    "link",
    "menu",
    "menu item",
    "check menu item",
    "radio menu item",
    "page tab",
];
/// The actions that press an element, in the words toolkits give them.
const PRESSES: &[&str] = &["click", "press", "activate", "toggle", "jump", "open"];

/// Keysyms of the keys the tool names and the modifiers.
const RETURN: u32 = 0xff0d;
const TAB: u32 = 0xff09;
const SHIFT: u32 = 0xffe1;
const CONTROL: u32 = 0xffe3;
const ALT: u32 = 0xffe9;
const SUPER: u32 = 0xffeb;

/// One process, as the process table has it.
#[derive(Clone, Debug, PartialEq)]
struct Proc {
    pid: u32,
    ppid: u32,
    name: String,
}

/// What one element says about itself, as a listing walks.
#[derive(Clone, Debug, Default, PartialEq)]
struct Node {
    role: String,
    name: String,
    description: String,
    value: Option<String>,
    frame: Option<Bounds>,
    showing: bool,
}

/// The Linux backend, one for the program, so what it has seen is shared.
pub(super) fn backend() -> Option<Arc<dyn Backend>> {
    static LINUX: OnceLock<Arc<Linux>> = OnceLock::new();
    let linux: Arc<dyn Backend> = LINUX.get_or_init(|| Arc::new(Linux::default())).clone();
    Some(linux)
}

#[derive(Default)]
struct Linux {
    apps: Mutex<Option<(Instant, Vec<App>)>>,
    hosts: Mutex<Option<(Instant, Vec<u32>)>>,
    known: Mutex<Known>,
    /// Held while reading elements or acting, so that two sessions' events
    /// never interleave.
    busy: Mutex<()>,
}

impl Backend for Linux {
    fn apps(&self) -> Result<Vec<App>, String> {
        let mut cached = lock(&self.apps);
        if let Some((at, apps)) = cached.as_ref()
            && at.elapsed() < APPS_FRESH
        {
            return Ok(apps.clone());
        }
        let apps = apps_of(&connect()?.windows()?, identity);
        *cached = Some((Instant::now(), apps.clone()));
        Ok(apps)
    }

    fn hosts(&self) -> Vec<u32> {
        let mut cached = lock(&self.hosts);
        if let Some((at, hosts)) = cached.as_ref()
            && at.elapsed() < HOSTS_FRESH
        {
            return hosts.clone();
        }
        let hosts = hosts_of(std::process::id(), &processes());
        *cached = Some((Instant::now(), hosts.clone()));
        hosts
    }

    fn screenshot(&self, app: Option<&App>) -> Result<Shot, String> {
        let x = connect()?;
        let screen = x.screen();
        let Some(app) = app else {
            let picture = x.picture(x.root(), screen)?;
            return Ok(Shot {
                picture: encode(picture)?,
                at: Point { x: 0.0, y: 0.0 },
            });
        };
        // Bottom first, so each window is drawn over those it is above.
        let shown: Vec<(Window, Bounds)> = own_windows(&x.windows()?, app, identity)
            .into_iter()
            .rev()
            .filter(|window| overlaps(window.bounds, screen))
            .map(|window| {
                let part = intersection(window.bounds, screen);
                (window, part)
            })
            .collect();
        let area = union(shown.iter().map(|(_, part)| *part))
            .ok_or_else(|| format!("{} has no window on screen.", app.name))?;
        let mut canvas = Rgb {
            width: area.width as u32,
            height: area.height as u32,
            pixels: vec![0; area.width as usize * area.height as usize * 3],
        };
        let mut drawn = 0;
        for (window, part) in &shown {
            let inside = Bounds {
                x: part.x - window.bounds.x,
                y: part.y - window.bounds.y,
                ..*part
            };
            // A window closed since the listing is just left out.
            if let Ok(picture) = x.picture(window.id, inside) {
                blit(&mut canvas, &picture, part.x - area.x, part.y - area.y);
                drawn += 1;
            }
        }
        if drawn == 0 {
            return Err(format!(
                "{}'s windows closed before a picture was taken.",
                app.name
            ));
        }
        Ok(Shot {
            picture: encode(canvas)?,
            at: Point {
                x: area.x,
                y: area.y,
            },
        })
    }

    fn elements(&self, app: &App) -> Result<Vec<Element>, String> {
        let _busy = lock(&self.busy);
        let bus = Bus::open(APP_ANSWERS)?;
        let (window, view) = front_window(&bus, app)?;
        let deadline = Instant::now() + WALK_TIME;
        let found = walk(
            window,
            view,
            |element| read(&bus, element),
            |element, _| bus.children(element),
            || Instant::now() < deadline,
        );
        let mut known = lock(&self.known);
        Ok(found
            .into_iter()
            .map(|(handle, mut element)| {
                element.key = known.key_for(handle, &element.role);
                element
            })
            .collect())
    }

    fn app_at(&self, point: Point) -> Result<Option<App>, String> {
        let x = connect()?;
        let Some(top) = x.top_at(point)? else {
            return Ok(None);
        };
        let windows = x.windows()?;
        owner(&x, &windows, top)
    }

    fn act(&self, app: &App, act: &Act) -> Result<(), String> {
        let _busy = lock(&self.busy);
        let x = connect()?;
        let windows = x.windows()?;
        let input = Input {
            x: &x,
            app,
            windows: own_windows(&windows, app, identity),
            others: others_frames(&windows, app, identity),
        };
        match act {
            Act::Click(Spot::Element(key)) => {
                let bus = Bus::open(APP_ANSWERS)?;
                let (element, read) = self.element(&bus, app, key)?;
                let actions = bus.actions(&element);
                let press = actions.iter().position(|action| {
                    PRESSES
                        .iter()
                        .any(|press| action.eq_ignore_ascii_case(press))
                });
                if let Some(press) = press {
                    return if bus.act(&element, press) {
                        Ok(())
                    } else {
                        Err(format!("{} would not press that element.", app.name))
                    };
                }
                input.click(centre(read.frame)?)
            }
            Act::Click(Spot::Point(at)) => input.click(*at),
            Act::Type { text, into } => {
                match into {
                    Some(Spot::Element(key)) => {
                        let bus = Bus::open(APP_ANSWERS)?;
                        let (element, read) = self.element(&bus, app, key)?;
                        let plain = !text.contains(['\n', '\r', '\t']);
                        if plain && read.offers(atspi::EDITABLE_TEXT) {
                            let focused = bus.focus(&element);
                            if bus.insert(&element, text) {
                                return Ok(());
                            }
                            if !focused {
                                input.click(centre(read.frame)?)?;
                            }
                        } else if !bus.focus(&element) {
                            // Keys go to what has the keyboard, so the
                            // element takes it by a click where AT-SPI
                            // cannot give it.
                            input.click(centre(read.frame)?)?;
                        }
                    }
                    Some(Spot::Point(at)) => input.click(*at)?,
                    None => {}
                }
                input.type_keys(text)
            }
            Act::Key { key, held } => input.press(key, held),
            Act::Scroll {
                at,
                direction,
                lines,
            } => {
                let at = match at {
                    Some(Spot::Element(key)) => {
                        let bus = Bus::open(APP_ANSWERS)?;
                        centre(self.element(&bus, app, key)?.1.frame)?
                    }
                    Some(Spot::Point(at)) => *at,
                    None => {
                        let front = input.windows.first();
                        let front = front
                            .ok_or_else(|| format!("{} has no window on screen.", app.name))?;
                        centre(Some(front.bounds))?
                    }
                };
                input.scroll(at, *direction, *lines)
            }
        }
    }
}

impl Linux {
    /// The element listed under `key`, if it is still `app`'s and on screen.
    fn element(
        &self,
        bus: &Bus,
        app: &App,
        key: &str,
    ) -> Result<(Accessible, atspi::Read), String> {
        let known = lock(&self.known).get(key).cloned();
        let (element, role) =
            known.ok_or("That element is no longer known: list the app's elements again.")?;
        let gone = || "That element is no longer on screen: list the app's elements again.";
        match bus.pid_of(&element.bus) {
            Some(pid) if app.pids.contains(&pid) => {}
            Some(_) => return Err(format!("That element is not one of {}'s.", app.name)),
            None => return Err(gone().into()),
        }
        let read = bus.read(&element).ok_or_else(gone)?;
        // An app may give a gone element's path to a new one, which is not
        // the one listed.
        if role_name(&role_of(&read)) != role || !read.has(atspi::SHOWING) {
            return Err(gone().into());
        }
        Ok((element, read))
    }
}

/// The X display, refused first where the session is Wayland's alone.
fn connect() -> Result<X, String> {
    let display = std::env::var("DISPLAY").ok();
    let wayland = std::env::var("WAYLAND_DISPLAY").ok();
    session(display.as_deref(), wayland.as_deref())?;
    X::connect()
}

/// Why computer use cannot run in a session with `display` and `wayland`
/// set as they are, or Ok where there is an X display to use.
fn session(display: Option<&str>, wayland: Option<&str>) -> Result<(), String> {
    let set = |value: Option<&str>| value.is_some_and(|value| !value.trim().is_empty());
    if set(display) {
        return Ok(());
    }
    let why = if set(wayland) {
        "Computer use needs X11 or XWayland, and this is a Wayland session with \
         no X display (DISPLAY is not set). Wayland lets a program see the screen \
         and make input only through the desktop's portals, which Letronna does \
         not use: the user can log in to an X11 session, or start Letronna with \
         DISPLAY set to XWayland's, which reaches the apps running on it. Carry \
         on without it."
    } else {
        "Computer use needs a desktop, and no X display is set (DISPLAY is \
         empty), as over SSH or on a server. Carry on without it."
    };
    Err(why.into())
}

/// The windows of `app`'s that can be seen, the one on top first. One
/// process can show several apps, as ssh does every app it forwards, so a
/// window is the app's only where `identify` gives it the app's id too.
fn own_windows(
    windows: &[Window],
    app: &App,
    mut identify: impl FnMut(&Window) -> String,
) -> Vec<Window> {
    windows
        .iter()
        .filter(|window| !window.panel && visible(window))
        .filter(|window| window.pid.is_some_and(|pid| app.pids.contains(&pid)))
        .filter(|window| identify(window).eq_ignore_ascii_case(&app.id))
        .cloned()
        .collect()
}

/// The frames of the listed windows that are not `app`'s, which a process
/// it shares with another app does not make its own.
fn others_frames(
    windows: &[Window],
    app: &App,
    mut identify: impl FnMut(&Window) -> String,
) -> Vec<u32> {
    windows
        .iter()
        .filter(|window| !identify(window).eq_ignore_ascii_case(&app.id))
        .map(|window| window.frame)
        .collect()
}

/// The app whose window is `top`, a window just under the root: the app
/// that window frames, else the process that made it, as a menu is.
fn owner(x: &X, windows: &[Window], top: u32) -> Result<Option<App>, String> {
    if let Some(window) = windows.iter().find(|window| window.frame == top) {
        let id = identity(window);
        let listed = apps_of(windows, identity)
            .into_iter()
            .find(|app| app.id.eq_ignore_ascii_case(&id));
        return Ok(Some(listed.unwrap_or_else(|| App {
            id,
            name: name_of(window),
            pids: window.pid.into_iter().collect(),
        })));
    }
    let pid = x.pid_of(top)?;
    let listed = apps_of(windows, identity)
        .into_iter()
        .find(|app| pid.is_some_and(|pid| app.pids.contains(&pid)));
    if listed.is_some() {
        return Ok(listed);
    }
    // A window with no app to name, such as a window manager's own: an id
    // no app has, so nothing is ever let through to it.
    let id = pid
        .and_then(program_of)
        .unwrap_or_else(|| format!("window {top:#x}"));
    Ok(Some(App {
        name: id.clone(),
        id,
        pids: pid.into_iter().collect(),
    }))
}

/// Made-up input to one app, checked before each event.
struct Input<'a> {
    x: &'a X,
    app: &'a App,
    /// The app's windows that can be seen, the one on top first.
    windows: Vec<Window>,
    /// The frames of the other apps' windows.
    others: Vec<u32>,
}

impl Input<'_> {
    /// Refuses unless `app` has the keyboard and, where `at` is given, its
    /// window is on top there, as the X server says right now.
    fn still_there(&self, at: Option<Point>) -> Result<(), String> {
        let focused = self.x.focused_top()?;
        let front = match focused {
            Some(top) => self.owns(top)?,
            None => false,
        };
        in_front(self.app, front)?;
        let Some(at) = at else {
            return Ok(());
        };
        let (x, y) = (at.x.round() as i64, at.y.round() as i64);
        match self.x.top_at(at)? {
            Some(top) if self.owns(top)? => Ok(()),
            Some(_) => Err(format!(
                "Stopped: another window is on top at {x},{y}, not {}'s, so nothing \
                 more was sent there. Take a screenshot to see what is.",
                self.app.name
            )),
            None => Err(format!(
                "Stopped: no window of {}'s is at {x},{y}, so nothing more was sent there.",
                self.app.name
            )),
        }
    }

    /// Whether `top`, a window just under the root, is one of the app's: a
    /// frame around one of its windows, or one its process made itself, as
    /// its menus are. A window that came up since the call began from any
    /// other process, the window manager's frame around a new one included,
    /// is not.
    fn owns(&self, top: u32) -> Result<bool, String> {
        if self.windows.iter().any(|window| window.frame == top) {
            return Ok(true);
        }
        if self.others.contains(&top) {
            return Ok(false);
        }
        let pid = self.x.pid_of(top)?;
        Ok(pid.is_some_and(|pid| self.app.pids.contains(&pid)))
    }

    /// Brings the app's top window to the front, waiting until it is there.
    fn to_front(&self) -> Result<(), String> {
        if self.still_there(None).is_ok() {
            return Ok(());
        }
        let window = self.windows.first();
        let window = window.ok_or_else(|| format!("{} has no window on screen.", self.app.name))?;
        self.x.activate(window)?;
        let deadline = Instant::now() + FRONT_WAIT;
        while Instant::now() < deadline {
            std::thread::sleep(FRONT_POLL);
            if self.still_there(None).is_ok() {
                return Ok(());
            }
        }
        Err(format!(
            "{} did not come to the front, so nothing was sent to it.",
            self.app.name
        ))
    }

    /// Presses `down` in order, then lets them go in the other order, each
    /// press checked first. What went down is let go even when a check
    /// fails: a release types nothing, and a key left down would repeat.
    fn together(&self, at: Option<Point>, down: &[Down]) -> Result<(), String> {
        let mut pressed = Vec::new();
        let mut checked = Ok(());
        for &event in down {
            checked = self.still_there(at);
            if checked.is_err() {
                break;
            }
            if let Err(error) = event.send(self.x, true) {
                checked = Err(error);
                break;
            }
            pressed.push(event);
            std::thread::sleep(EVENT_GAP);
        }
        if checked.is_ok() {
            checked = self.still_there(at);
        }
        let mut released = Ok(());
        for event in pressed.into_iter().rev() {
            if let Err(error) = event.send(self.x, false) {
                released = Err(error);
            }
            std::thread::sleep(EVENT_GAP);
        }
        checked.and(released)
    }

    /// Clicks at `at`, the app in front and its window there.
    fn click(&self, at: Point) -> Result<(), String> {
        self.to_front()?;
        self.still_there(Some(at))?;
        self.x.motion(at)?;
        std::thread::sleep(EVENT_GAP);
        self.together(Some(at), &[Down::Button(1)])
    }

    /// Scrolls `lines` towards `direction` at `at`, a wheel click a line.
    fn scroll(&self, at: Point, direction: Direction, lines: u32) -> Result<(), String> {
        // X's wheel is buttons 4 to 7: up, down, left, right.
        let button = match direction {
            Direction::Up => 4,
            Direction::Down => 5,
            Direction::Left => 6,
            Direction::Right => 7,
        };
        self.to_front()?;
        self.still_there(Some(at))?;
        self.x.motion(at)?;
        std::thread::sleep(EVENT_GAP);
        for _ in 0..lines {
            self.together(Some(at), &[Down::Button(button)])?;
        }
        Ok(())
    }

    /// Types `text` with made-up keys, the app in front. A character no key
    /// types is lent a key with none, as xdotool does, which is given back
    /// before this returns.
    fn type_keys(&self, text: &str) -> Result<(), String> {
        self.to_front()?;
        let keymap = self.x.keymap()?;
        let spare = keymap.spare();
        let mut lent = None;
        let typed = self.type_with(&keymap, spare, &mut lent, text);
        if lent.is_some()
            && let Some(spare) = spare
        {
            std::thread::sleep(LEND_GAP);
            // Failing, it leaves one key no layout uses typing a character.
            let _ = self.x.remap(spare, 0);
        }
        typed
    }

    fn type_with(
        &self,
        keymap: &Keymap,
        spare: Option<u8>,
        lent: &mut Option<u32>,
        text: &str,
    ) -> Result<(), String> {
        let shift = keymap.code_for(SHIFT).map(|(code, _)| code);
        for c in text.replace("\r\n", "\n").chars() {
            let sym = char_sym(c);
            let (code, shifted) = match keymap.code_for(sym) {
                Some(found) => found,
                None => {
                    let spare = spare.ok_or_else(|| {
                        format!(
                            "`{c}` cannot be typed: no key types it, and none is free to lend it."
                        )
                    })?;
                    if *lent != Some(sym) {
                        self.x.remap(spare, sym)?;
                        *lent = Some(sym);
                        std::thread::sleep(LEND_GAP);
                    }
                    (spare, false)
                }
            };
            let mut down = Vec::new();
            if shifted {
                down.push(Down::Key(
                    shift.ok_or("The keyboard has no Shift key to type capitals with.")?,
                ));
            }
            down.push(Down::Key(code));
            self.together(None, &down)?;
        }
        Ok(())
    }

    /// Presses `key` with `held` down, the app in front; a character with no
    /// modifier is typed as itself.
    fn press(&self, key: &str, held: &[Modifier]) -> Result<(), String> {
        if let Some(what) = system_shortcut(key, held) {
            return Err(format!(
                "Refused: that shortcut {what}, which acts on the whole desktop rather \
                 than {}, so Letronna never presses it.",
                self.app.name
            ));
        }
        if key.chars().count() == 1 && held.is_empty() {
            return self.type_keys(key);
        }
        let keymap = self.x.keymap()?;
        let sym = keysym_of(key).ok_or_else(|| format!("`{key}` is not a key."))?;
        let (code, shifted) = keymap.code_for(sym).ok_or_else(|| {
            format!("`{key}` cannot be pressed with modifiers: no key on this keyboard types it.")
        })?;
        let mut down = Vec::new();
        let mut modifiers = held.to_vec();
        if shifted && !modifiers.contains(&Modifier::Shift) {
            modifiers.push(Modifier::Shift);
        }
        for modifier in modifiers {
            let sym = match modifier {
                Modifier::Command => SUPER,
                Modifier::Control => CONTROL,
                Modifier::Alt => ALT,
                Modifier::Shift => SHIFT,
            };
            let (code, _) = keymap
                .code_for(sym)
                .ok_or_else(|| format!("The keyboard has no {modifier:?} key to hold."))?;
            down.push(Down::Key(code));
        }
        down.push(Down::Key(code));
        self.to_front()?;
        self.together(None, &down)
    }
}

/// A key or button that goes down, then up.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Down {
    Key(u8),
    Button(u8),
}

impl Down {
    fn send(self, x: &X, down: bool) -> Result<(), String> {
        match self {
            Down::Key(code) => x.key(code, down),
            Down::Button(button) => x.button(button, down),
        }
    }
}

/// Refuses unless `app` has the keyboard, `front` saying whether it does.
fn in_front(app: &App, front: bool) -> Result<(), String> {
    if front {
        return Ok(());
    }
    Err(format!(
        "Stopped: {} is not in front, so nothing more was sent. Take a \
         screenshot to see what is.",
        app.name
    ))
}

/// `app`'s window in front on the accessibility bus, and where it is: the
/// one active, else the first on screen.
fn front_window(bus: &Bus, app: &App) -> Result<(Accessible, Bounds), String> {
    let mut first = None;
    for root in bus.apps() {
        if !bus
            .pid_of(&root.bus)
            .is_some_and(|pid| app.pids.contains(&pid))
        {
            continue;
        }
        for window in bus.children(&root) {
            let Some(read) = bus.read(&window) else {
                continue;
            };
            let Some(frame) = read.frame.filter(|f| f.width > 0.0 && f.height > 0.0) else {
                continue;
            };
            if read.has(atspi::ACTIVE) {
                return Ok((window, frame));
            }
            if first.is_none() && read.has(atspi::SHOWING) {
                first = Some((window, frame));
            }
        }
    }
    first.ok_or_else(|| {
        format!(
            "{} shares no elements over AT-SPI, the accessibility bus: it may not \
             support it, or turn it on only while a screen reader runs. Use \
             screenshot, and click or scroll by spot, instead.",
            app.name
        )
    })
}

/// What `element` says about itself, as a listing walks.
fn read(bus: &Bus, element: &Accessible) -> Option<Node> {
    let read = bus.read(element)?;
    let value = bus.value(element, &read, TEXT_KEPT);
    Some(Node {
        showing: read.has(atspi::SHOWING),
        role: role_of(&read),
        name: read.name,
        description: read.description,
        value,
        frame: read.frame,
    })
}

/// `read`'s role, where GTK calls a text field plain text and says only
/// that it can be edited.
fn role_of(read: &atspi::Read) -> String {
    if read.role == "text" && read.has(atspi::EDITABLE) {
        return "entry".into();
    }
    read.role.clone()
}

/// The tree under `root` flattened in the order a screen reader meets it:
/// what has a name, a value or a use, and is on screen within `view` and
/// the scroll panes it sits in. What is not showing or is out of view is
/// skipped with all it holds, and the walk stops at `LISTED_MOST` found,
/// `READ_MOST` read, `DEPTH_MOST` deep, or when `in_time` says so.
fn walk<N>(
    root: N,
    view: Bounds,
    mut read: impl FnMut(&N) -> Option<Node>,
    mut children: impl FnMut(&N, &Node) -> Vec<N>,
    mut in_time: impl FnMut() -> bool,
) -> Vec<(N, Element)> {
    let mut found = Vec::new();
    let mut stack = vec![(root, 0, view)];
    let mut reads = 0;
    while let Some((handle, depth, view)) = stack.pop() {
        if found.len() >= LISTED_MOST || reads >= READ_MOST || !in_time() {
            break;
        }
        reads += 1;
        let Some(node) = read(&handle) else {
            continue;
        };
        if !node.showing {
            continue;
        }
        // A container can report no size while its children have one.
        let frame = node.frame.filter(|f| f.width > 0.0 && f.height > 0.0);
        if frame.is_some_and(|frame| !overlaps(frame, view)) {
            continue;
        }
        let inner = match frame {
            Some(frame) if matches!(node.role.as_str(), "scroll pane" | "viewport") => {
                intersection(frame, view)
            }
            _ => view,
        };
        if depth < DEPTH_MOST {
            let below = children(&handle, &node);
            stack.extend(
                below
                    .into_iter()
                    .rev()
                    .map(|child| (child, depth + 1, inner)),
            );
        }
        if let Some(frame) = frame
            && let Some(element) = element_of(&node, frame)
        {
            found.push((handle, element));
        }
    }
    found
}

/// `node` as the tool lists it, or None where it has nothing to show: no
/// name, no value and nothing it can be used for. A password field's value
/// is never given.
fn element_of(node: &Node, frame: Bounds) -> Option<Element> {
    let name = [&node.name, &node.description]
        .into_iter()
        .map(|text| text.trim())
        .find(|text| !text.is_empty())
        .unwrap_or_default();
    let value = node
        .value
        .as_deref()
        .filter(|_| node.role != atspi::PASSWORD)
        .filter(|value| !value.trim().is_empty())
        .map(|value| value.chars().take(TEXT_KEPT).collect::<String>());
    if name.is_empty() && value.is_none() && !ACTIONABLE.contains(&node.role.as_str()) {
        return None;
    }
    Some(Element {
        key: String::new(),
        role: role_name(&node.role),
        name: name.chars().take(TEXT_KEPT).collect(),
        value,
        bounds: frame,
    })
}

/// A role as the other systems' backends word it, where AT-SPI's own words
/// differ: `push button` is a button, `entry` a text field.
fn role_name(role: &str) -> String {
    let shown = match role {
        "push button" => "button",
        "entry" => "text field",
        "password text" => "password field",
        "label" | "static" => "text",
        "" => "element",
        other => other,
    };
    shown.to_owned()
}

/// The elements listed so far, under keys that stay theirs: an element met
/// again, at the same place on the bus with the same role, gets its old
/// key, and a key is never given to another element, so a stale one finds
/// nothing.
#[derive(Default)]
struct Known {
    next: u64,
    by_element: HashMap<Accessible, u64>,
    /// Each key's element and the role it had when listed.
    by_key: BTreeMap<u64, (Accessible, String)>,
}

impl Known {
    /// `element`'s key, a new one the first time it is met with `role`.
    fn key_for(&mut self, element: Accessible, role: &str) -> String {
        let known = self
            .by_element
            .get(&element)
            .copied()
            .filter(|key| self.by_key.get(key).is_some_and(|(_, had)| had == role));
        if let Some(key) = known {
            return format!("at{key}");
        }
        if self.by_key.len() >= KNOWN_KEPT {
            // The oldest go first: the model has long moved on from them.
            let kept_from = self.next.saturating_sub(KNOWN_KEPT as u64 / 2);
            self.by_key = self.by_key.split_off(&kept_from);
            let kept = &self.by_key;
            self.by_element.retain(|_, key| kept.contains_key(key));
        }
        self.next += 1;
        self.by_element.insert(element.clone(), self.next);
        self.by_key.insert(self.next, (element, role.to_owned()));
        format!("at{}", self.next)
    }

    /// The element listed under `key` and the role it was listed with.
    fn get(&self, key: &str) -> Option<&(Accessible, String)> {
        let number = key.strip_prefix("at")?.parse().ok()?;
        self.by_key.get(&number)
    }
}

/// The apps with a window on screen, the one whose window is on top first,
/// an app's processes together under the id `identify` gives. Panels and
/// the desktop's background are left out, as the Dock is on macOS.
fn apps_of(windows: &[Window], mut identify: impl FnMut(&Window) -> String) -> Vec<App> {
    let mut apps: Vec<App> = Vec::new();
    for window in windows
        .iter()
        .filter(|window| !window.panel && visible(window))
    {
        let id = identify(window);
        match apps.iter_mut().find(|app| app.id.eq_ignore_ascii_case(&id)) {
            Some(app) => {
                if let Some(pid) = window.pid
                    && !app.pids.contains(&pid)
                {
                    app.pids.push(pid);
                }
            }
            None => apps.push(App {
                name: name_of(window),
                id,
                pids: window.pid.into_iter().collect(),
            }),
        }
    }
    apps
}

/// What an app is called on screen: its class, else its window's title.
fn name_of(window: &Window) -> String {
    [&window.class, &window.title, &window.instance]
        .into_iter()
        .map(|name| name.trim())
        .find(|name| !name.is_empty())
        .map_or_else(|| format!("window {:#x}", window.id), str::to_owned)
}

/// The id grants and rules know `window`'s app by: the name of the program
/// its process runs, as on Windows; its class's instance only where there
/// is no process to look at, as for an app across the network, or where
/// its class names an app that is never controlled.
fn identity(window: &Window) -> String {
    let program = window.pid.and_then(program_of);
    let claimed: Vec<&str> = [&window.instance, &window.class]
        .into_iter()
        .map(|name| name.trim())
        .filter(|name| !name.is_empty())
        .collect();
    // Terminator and Guake run under Python, urxvtc's windows are urxvtd's
    // and a forwarded xterm's are ssh's, so their program names no
    // terminal. A class is only the app's own claim, but one that names an
    // app never controlled can get no app refused but the claimant.
    let refused = claimed.iter().find(|name| out_of_reach(name));
    match (program, refused) {
        (Some(program), _) if out_of_reach(&program) => program,
        (_, Some(refused)) => (*refused).to_owned(),
        (Some(program), None) => program,
        (None, None) => claimed.first().map_or_else(
            || format!("window {:#x}", window.id),
            |name| (*name).to_owned(),
        ),
    }
}

/// Whether `id` is one of the apps core never lets a call control.
fn out_of_reach(id: &str) -> bool {
    [super::LETRONNA, super::TERMINALS, super::PROMPTS]
        .into_iter()
        .any(|ids| super::one_of(id, ids))
}

/// The file name of the program process `pid` runs, or its short name where
/// the program cannot be looked at.
fn program_of(pid: u32) -> Option<String> {
    if let Ok(path) = std::fs::read_link(format!("/proc/{pid}/exe")) {
        let name = path.file_name()?.to_string_lossy().into_owned();
        // A program replaced by an update while it runs.
        return Some(name.trim_end_matches(" (deleted)").to_owned());
    }
    let comm = std::fs::read_to_string(format!("/proc/{pid}/comm")).ok()?;
    Some(comm.trim().to_owned()).filter(|comm| !comm.is_empty())
}

/// Whether `window` is more than a sliver.
fn visible(window: &Window) -> bool {
    window.bounds.width >= 2.0 && window.bounds.height >= 2.0
}

fn overlaps(a: Bounds, b: Bounds) -> bool {
    a.x < b.x + b.width && b.x < a.x + a.width && a.y < b.y + b.height && b.y < a.y + a.height
}

/// Where `a` and `b` overlap; call only when they do.
fn intersection(a: Bounds, b: Bounds) -> Bounds {
    let (x, y) = (a.x.max(b.x), a.y.max(b.y));
    Bounds {
        x,
        y,
        width: (a.x + a.width).min(b.x + b.width) - x,
        height: (a.y + a.height).min(b.y + b.height) - y,
    }
}

/// The smallest rectangle holding all of `all`, or None for none.
fn union(all: impl IntoIterator<Item = Bounds>) -> Option<Bounds> {
    all.into_iter().reduce(|a, b| {
        let (x, y) = (a.x.min(b.x), a.y.min(b.y));
        Bounds {
            x,
            y,
            width: (a.x + a.width).max(b.x + b.width) - x,
            height: (a.y + a.height).max(b.y + b.height) - y,
        }
    })
}

/// The middle of `frame`, where a click on an element lands.
fn centre(frame: Option<Bounds>) -> Result<Point, String> {
    frame
        .filter(|frame| frame.width > 0.0 && frame.height > 0.0)
        .map(|frame| Point {
            x: frame.x + frame.width / 2.0,
            y: frame.y + frame.height / 2.0,
        })
        .ok_or_else(|| "That element has no place on screen to click.".into())
}

/// Draws `picture` onto `canvas` with its top left corner at `x`,`y`,
/// cutting what falls outside.
fn blit(canvas: &mut Rgb, picture: &Rgb, x: f64, y: f64) {
    let (left, top) = (x.max(0.0) as usize, y.max(0.0) as usize);
    let (canvas_width, canvas_height) = (canvas.width as usize, canvas.height as usize);
    let width = (picture.width as usize).min(canvas_width.saturating_sub(left));
    for row in 0..(picture.height as usize).min(canvas_height.saturating_sub(top)) {
        let from = row * picture.width as usize * 3;
        let to = ((top + row) * canvas_width + left) * 3;
        canvas.pixels[to..to + width * 3].copy_from_slice(&picture.pixels[from..from + width * 3]);
    }
}

/// `picture` as a PNG, or a JPEG where the PNG is too large to send, as a
/// photo on the desktop can make it.
fn encode(picture: Rgb) -> Result<Vec<u8>, String> {
    use image::codecs::jpeg::JpegEncoder;
    let failed = |error: image::ImageError| format!("Could not make the picture: {error}.");
    let image = image::RgbImage::from_raw(picture.width, picture.height, picture.pixels)
        .ok_or("The picture came back the wrong size.")?;
    let mut png = Vec::new();
    image
        .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
        .map_err(failed)?;
    if png.len() <= crate::media::PICTURE_BYTES {
        return Ok(png);
    }
    for quality in [85, 60] {
        let mut jpeg = Vec::new();
        JpegEncoder::new_with_quality(&mut jpeg, quality)
            .encode_image(&image)
            .map_err(failed)?;
        if jpeg.len() <= crate::media::PICTURE_BYTES {
            return Ok(jpeg);
        }
    }
    let why = "The screenshot is too large to send even as a JPEG: take one of an \
               app instead of the whole screen.";
    Err(why.into())
}

/// The keysym a typed character is, return and tab as their keys.
fn char_sym(c: char) -> u32 {
    match c {
        '\n' | '\r' => RETURN,
        '\t' => TAB,
        // Latin-1's keysyms are its characters; the rest of Unicode sits
        // above 0x1000000.
        ' '..='~' | '\u{a0}'..='\u{ff}' => c.into(),
        _ => 0x0100_0000 + u32::from(c),
    }
}

/// The keysym of a key the tool names, or of one character.
fn keysym_of(key: &str) -> Option<u32> {
    let sym = match key {
        "enter" | "return" => RETURN,
        "tab" => TAB,
        "escape" => 0xff1b,
        "space" => 0x20,
        "backspace" => 0xff08,
        "delete" => 0xffff,
        "home" => 0xff50,
        "left" => 0xff51,
        "up" => 0xff52,
        "right" => 0xff53,
        "down" => 0xff54,
        "pageup" => 0xff55,
        "pagedown" => 0xff56,
        "end" => 0xff57,
        _ => {
            if let Some(n) = key.strip_prefix('f').and_then(|n| n.parse::<u32>().ok())
                && (1..=12).contains(&n)
            {
                // F1 is 0xffbe, and the rest follow it.
                return Some(0xffbd + n);
            }
            let mut chars = key.chars();
            let (Some(c), None) = (chars.next(), chars.next()) else {
                return None;
            };
            char_sym(c)
        }
    };
    Some(sym)
}

/// What `key` with `held` does to the whole desktop, if it is one of the
/// shortcuts X servers and desktops keep for themselves: ending the X
/// session, switching to a console, locking the screen, and opening a
/// terminal or a box that runs commands, which would get around
/// run_command's rules.
fn system_shortcut(key: &str, held: &[Modifier]) -> Option<&'static str> {
    use Modifier::{Alt, Command, Control};
    let has = |wanted: &[Modifier]| wanted.iter().all(|modifier| held.contains(modifier));
    let console = key
        .strip_prefix('f')
        .and_then(|n| n.parse::<u32>().ok())
        .is_some();
    match key {
        "backspace" if has(&[Control, Alt]) => Some("can end the whole X session"),
        "delete" if has(&[Control, Alt]) => Some("opens the log out prompt"),
        _ if console && has(&[Control, Alt]) => Some("switches to another console"),
        "t" if has(&[Control, Alt]) => Some("opens a terminal"),
        "l" if has(&[Control, Alt]) || has(&[Command]) => Some("locks the screen"),
        "f2" if has(&[Alt]) => Some("opens the desktop's box for running a command"),
        _ => None,
    }
}

/// Every process, as the process table has it now.
fn processes() -> Vec<Proc> {
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter(|entry| {
            entry
                .file_name()
                .to_string_lossy()
                .bytes()
                .all(|b| b.is_ascii_digit())
        })
        .filter_map(|entry| std::fs::read_to_string(entry.path().join("stat")).ok())
        .filter_map(|stat| parse_stat(&stat))
        .collect()
}

/// A process from its `/proc/<pid>/stat` line, whose name, in brackets, can
/// hold spaces and brackets itself.
fn parse_stat(stat: &str) -> Option<Proc> {
    let (pid, rest) = stat.split_once(" (")?;
    let (name, rest) = rest.rsplit_once(") ")?;
    let mut fields = rest.split_whitespace();
    let _state = fields.next()?;
    Some(Proc {
        pid: pid.trim().parse().ok()?,
        ppid: fields.next()?.parse().ok()?,
        name: name.to_owned(),
    })
}

/// The processes whose windows show `own`: its parents up to the terminal,
/// and where one is a terminal multiplexer such as tmux, whose server has
/// no terminal, the parents of each of its clients, in whichever terminal
/// they run. Every client of every server of that kind counts: a terminal
/// left out would be one Letronna could type into.
fn hosts_of(own: u32, procs: &[Proc]) -> Vec<u32> {
    let by_pid: HashMap<u32, &Proc> = procs.iter().map(|proc| (proc.pid, proc)).collect();
    let mut hosts = ancestors(own, &by_pid);
    let multiplexers: Vec<&str> = hosts
        .iter()
        .filter_map(|pid| by_pid.get(pid))
        .filter_map(|proc| multiplexer(&proc.name))
        .collect();
    for proc in procs {
        let client = multiplexer(&proc.name).is_some_and(|kind| multiplexers.contains(&kind));
        if !client || proc.pid == own || hosts.contains(&proc.pid) {
            continue;
        }
        hosts.push(proc.pid);
        for pid in ancestors(proc.pid, &by_pid) {
            if !hosts.contains(&pid) {
                hosts.push(pid);
            }
        }
    }
    hosts
}

/// Which of `MULTIPLEXERS` a process called `name` is, if one: tmux names
/// its processes `tmux: server` and `tmux: client`.
fn multiplexer(name: &str) -> Option<&'static str> {
    let name = name.to_lowercase();
    let program = name.split(':').next().unwrap_or_default().trim();
    MULTIPLEXERS
        .iter()
        .find(|&&known| known == program)
        .copied()
}

/// `pid`'s parents, nearest first, up to but leaving out init.
fn ancestors(pid: u32, by_pid: &HashMap<u32, &Proc>) -> Vec<u32> {
    let mut found = Vec::new();
    let mut at = pid;
    while found.len() < ANCESTORS_MOST {
        let Some(proc) = by_pid.get(&at) else {
            break;
        };
        let parent = proc.ppid;
        if parent <= 1 || parent == pid || found.contains(&parent) {
            break;
        }
        found.push(parent);
        at = parent;
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(x: f64, y: f64, width: f64, height: f64) -> Bounds {
        Bounds {
            x,
            y,
            width,
            height,
        }
    }

    fn window(id: u32, pid: Option<u32>, class: &str, bounds: Bounds) -> Window {
        Window {
            id,
            frame: id + 1000,
            pid,
            class: class.into(),
            instance: class.to_lowercase(),
            title: format!("{class} window"),
            bounds,
            panel: false,
        }
    }

    fn node(role: &str, name: &str, value: Option<&str>, frame: Option<Bounds>) -> Node {
        Node {
            role: role.into(),
            name: name.into(),
            value: value.map(str::to_owned),
            frame,
            showing: true,
            ..Node::default()
        }
    }

    /// A made-up accessibility tree: each element's node and children.
    struct Tree(HashMap<u32, (Node, Vec<u32>)>);

    impl Tree {
        fn new(items: Vec<(u32, Node, Vec<u32>)>) -> Tree {
            Tree(
                items
                    .into_iter()
                    .map(|(id, node, children)| (id, (node, children)))
                    .collect(),
            )
        }

        fn walk(&self, root: u32) -> Vec<(u32, Element)> {
            walk(
                root,
                rect(0.0, 0.0, 800.0, 600.0),
                |id| self.0.get(id).map(|(node, _)| node.clone()),
                |id, _| {
                    self.0
                        .get(id)
                        .map(|(_, below)| below.clone())
                        .unwrap_or_default()
                },
                || true,
            )
        }
    }

    #[test]
    fn a_wayland_session_with_no_x_display_is_refused_with_why() {
        assert_eq!(session(Some(":0"), None), Ok(()));
        // XWayland's display is used where there is one.
        assert_eq!(session(Some(":1"), Some("wayland-0")), Ok(()));
        let wayland = session(None, Some("wayland-0")).unwrap_err();
        assert!(wayland.contains("needs X11 or XWayland"), "{wayland}");
        assert!(wayland.contains("portals"), "{wayland}");
        let neither = session(Some(" "), None).unwrap_err();
        assert!(neither.contains("no X display is set"), "{neither}");
    }

    #[test]
    fn apps_come_top_first_with_an_apps_processes_together_and_panels_left_out() {
        let panel = Window {
            panel: true,
            ..window(1, Some(10), "Panel", rect(0.0, 0.0, 1280.0, 30.0))
        };
        let windows = [
            panel,
            window(2, Some(20), "Firefox", rect(0.0, 30.0, 800.0, 600.0)),
            window(3, Some(30), "XClock", rect(10.0, 10.0, 1.0, 1.0)),
            window(4, Some(21), "Firefox", rect(100.0, 30.0, 800.0, 600.0)),
            window(5, Some(40), "Gedit", rect(0.0, 30.0, 400.0, 300.0)),
        ];
        let apps = apps_of(&windows, |window| window.instance.clone());
        let listed: Vec<(&str, &str, &[u32])> = apps
            .iter()
            .map(|app| (app.id.as_str(), app.name.as_str(), app.pids.as_slice()))
            .collect();
        assert_eq!(
            listed,
            [
                ("firefox", "Firefox", &[20, 21][..]),
                ("gedit", "Gedit", &[40][..])
            ]
        );
        // An app with no class goes by its window's title, and with neither
        // by its window.
        let titled = Window {
            class: String::new(),
            title: "Event Tester".into(),
            ..window(6, Some(50), "", rect(0.0, 0.0, 300.0, 300.0))
        };
        let untitled = Window {
            title: String::new(),
            ..titled.clone()
        };
        assert_eq!(name_of(&untitled), "window 0x6");
        assert_eq!(name_of(&titled), "Event Tester");
    }

    #[test]
    fn an_app_is_known_by_its_program_else_by_its_class() {
        let own = window(
            1,
            Some(std::process::id()),
            "Mine",
            rect(0.0, 0.0, 9.0, 9.0),
        );
        let program = program_of(std::process::id()).unwrap();
        assert_eq!(identity(&own), program);
        // Across the network there is no process to look at.
        let remote = window(2, None, "XClock", rect(0.0, 0.0, 9.0, 9.0));
        assert_eq!(identity(&remote), "xclock");
        let bare = Window {
            class: String::new(),
            instance: String::new(),
            ..remote
        };
        assert_eq!(identity(&bare), "window 0x2");
    }

    #[test]
    fn a_terminal_whose_program_is_no_terminals_is_known_by_its_class() {
        // This test's own program stands for Python under Terminator.
        let pid = Some(std::process::id());
        let program = program_of(std::process::id()).unwrap();
        assert!(!out_of_reach(&program), "{program}");
        let terminator = window(1, pid, "Terminator", rect(0.0, 0.0, 9.0, 9.0));
        assert_eq!(identity(&terminator), "terminator");
        let urxvt = Window {
            instance: "urxvt".into(),
            ..window(2, pid, "URxvt", rect(0.0, 0.0, 9.0, 9.0))
        };
        assert_eq!(identity(&urxvt), "urxvt");
        let askpass = window(3, None, "Pinentry-gnome3", rect(0.0, 0.0, 9.0, 9.0));
        assert_eq!(identity(&askpass), "pinentry-gnome3");
        // Any other class leaves the program as the id.
        let editor = window(4, pid, "Gedit", rect(0.0, 0.0, 9.0, 9.0));
        assert_eq!(identity(&editor), program);
    }

    #[test]
    fn only_the_apps_own_windows_are_drawn_or_acted_on() {
        let app = App {
            id: "firefox".into(),
            name: "Firefox".into(),
            pids: vec![20],
        };
        let windows = [
            window(1, Some(30), "Mail", rect(0.0, 0.0, 400.0, 300.0)),
            window(2, Some(20), "Firefox", rect(0.0, 0.0, 800.0, 600.0)),
            window(3, None, "Firefox", rect(0.0, 0.0, 800.0, 600.0)),
            window(4, Some(20), "Firefox", rect(0.0, 0.0, 1.0, 600.0)),
        ];
        let by_instance = |window: &Window| window.instance.clone();
        let own: Vec<u32> = own_windows(&windows, &app, by_instance)
            .iter()
            .map(|w| w.id)
            .collect();
        assert_eq!(own, [2]);
        // ssh's one process shows every app it forwards: a forwarded xterm
        // is not the forwarded Firefox's.
        let forwarded = [
            window(5, Some(20), "XTerm", rect(0.0, 0.0, 400.0, 300.0)),
            window(6, Some(20), "Firefox", rect(0.0, 0.0, 800.0, 600.0)),
        ];
        let own: Vec<u32> = own_windows(&forwarded, &app, by_instance)
            .iter()
            .map(|w| w.id)
            .collect();
        assert_eq!(own, [6]);
        assert_eq!(others_frames(&forwarded, &app, by_instance), [1005]);
    }

    #[test]
    fn a_picture_is_drawn_where_its_window_is_and_cut_at_the_edges() {
        let mut canvas = Rgb {
            width: 3,
            height: 2,
            pixels: vec![0; 18],
        };
        let picture = Rgb {
            width: 2,
            height: 2,
            pixels: (1..=12).collect(),
        };
        blit(&mut canvas, &picture, 2.0, 1.0);
        assert_eq!(
            canvas.pixels,
            [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1, 2, 3]
        );
        let area = union([rect(10.0, 10.0, 5.0, 5.0), rect(0.0, 20.0, 5.0, 5.0)]);
        assert_eq!(area, Some(rect(0.0, 10.0, 15.0, 15.0)));
        let screen = rect(0.0, 0.0, 100.0, 100.0);
        let part = intersection(rect(-10.0, 90.0, 50.0, 50.0), screen);
        assert_eq!(part, rect(0.0, 90.0, 40.0, 10.0));
        assert!(!overlaps(rect(100.0, 0.0, 5.0, 5.0), screen));
    }

    #[test]
    fn a_screenshot_is_a_png_until_that_is_too_large_to_send() {
        let flat = Rgb {
            width: 64,
            height: 32,
            pixels: vec![200; 64 * 32 * 3],
        };
        let png = encode(flat).unwrap();
        assert_eq!(crate::media::picture_kind(&png), Some("png"));
        // Noise barely shrinks as a PNG; as a JPEG it does.
        let (width, height) = (1400u32, 1000u32);
        let mut seed = 1u32;
        let noise = (0..width * height * 3)
            .map(|_| {
                seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                (seed >> 24) as u8
            })
            .collect();
        let noisy = Rgb {
            width,
            height,
            pixels: noise,
        };
        let jpeg = encode(noisy).unwrap();
        assert_eq!(crate::media::picture_kind(&jpeg), Some("jpg"));
        assert!(jpeg.len() <= crate::media::PICTURE_BYTES);
    }

    #[test]
    fn a_listing_reads_in_order_and_skips_what_is_not_showing() {
        let frame = Some(rect(0.0, 0.0, 800.0, 600.0));
        let button = Some(rect(10.0, 10.0, 80.0, 20.0));
        let hidden = Node {
            showing: false,
            ..node("page tab", "Hidden tab", None, button)
        };
        let tree = Tree::new(vec![
            (1, node("frame", "Notes", None, frame), vec![2, 5, 6]),
            (2, node("panel", "", None, frame), vec![3, 4]),
            (3, node("push button", "Save", None, button), vec![]),
            (4, node("entry", "", Some("draft"), button), vec![]),
            (5, hidden, vec![7]),
            (6, node("label", "Ready", None, None), vec![]),
            (
                7,
                node("push button", "Inside hidden", None, button),
                vec![],
            ),
        ]);
        let found: Vec<(u32, String, String)> = tree
            .walk(1)
            .into_iter()
            .map(|(id, element)| (id, element.role, element.name))
            .collect();
        assert_eq!(
            found,
            [
                (1, "frame".into(), "Notes".into()),
                (3, "button".into(), "Save".into()),
                (4, "text field".into(), String::new()),
            ]
        );
    }

    #[test]
    fn what_is_scrolled_out_of_view_is_skipped_with_all_it_holds() {
        let pane = Some(rect(0.0, 0.0, 800.0, 100.0));
        let tree = Tree::new(vec![
            (1, node("scroll pane", "", None, pane), vec![2, 3]),
            (
                2,
                node(
                    "list item",
                    "Seen",
                    None,
                    Some(rect(0.0, 50.0, 800.0, 20.0)),
                ),
                vec![],
            ),
            (
                3,
                node(
                    "list item",
                    "Below",
                    None,
                    Some(rect(0.0, 300.0, 800.0, 20.0)),
                ),
                vec![4],
            ),
            (
                4,
                node(
                    "push button",
                    "Inside",
                    None,
                    Some(rect(0.0, 300.0, 9.0, 9.0)),
                ),
                vec![],
            ),
        ]);
        let names: Vec<String> = tree.walk(1).into_iter().map(|(_, e)| e.name).collect();
        assert_eq!(names, ["Seen"]);
    }

    #[test]
    fn a_password_fields_value_is_never_listed() {
        let at = rect(0.0, 0.0, 9.0, 9.0);
        let password = node(atspi::PASSWORD, "Password", Some("hunter2"), Some(at));
        let element = element_of(&password, at).unwrap();
        assert_eq!(element.role, "password field");
        assert_eq!(element.value, None);
        let read = atspi::Read {
            role: "text".into(),
            states: 1 << atspi::EDITABLE,
            ..atspi::Read::default()
        };
        assert_eq!(role_name(&role_of(&read)), "text field");
        let label = atspi::Read {
            role: "text".into(),
            ..atspi::Read::default()
        };
        assert_eq!(role_name(&role_of(&label)), "text");
        // What shows nothing and does nothing is left out.
        assert_eq!(element_of(&node("filler", "", None, Some(at)), at), None);
    }

    #[test]
    fn a_listing_stops_at_its_limits() {
        let at = Some(rect(0.0, 0.0, 9.0, 9.0));
        let many: Vec<u32> = (2..=LISTED_MOST as u32 + 50).collect();
        let mut items = vec![(1, node("frame", "Busy", None, at), many.clone())];
        items.extend(
            many.iter()
                .map(|&id| (id, node("push button", "Row", None, at), vec![])),
        );
        assert_eq!(Tree::new(items).walk(1).len(), LISTED_MOST);
        let mut reads = 0;
        let stopped = walk(
            1u32,
            rect(0.0, 0.0, 9.0, 9.0),
            |_| {
                reads += 1;
                Some(node("push button", "Deep", None, at))
            },
            |id, _| vec![id + 1],
            || true,
        );
        assert_eq!(stopped.len(), DEPTH_MOST + 1);
        assert_eq!(reads, DEPTH_MOST + 1);
    }

    #[test]
    fn an_element_met_again_keeps_its_key_and_no_key_is_given_twice() {
        let element = |path: &str| Accessible {
            bus: ":1.5".into(),
            path: path.into(),
        };
        let mut known = Known::default();
        let save = known.key_for(element("/a/1"), "button");
        let body = known.key_for(element("/a/2"), "text field");
        assert_eq!((save.as_str(), body.as_str()), ("at1", "at2"));
        assert_eq!(known.key_for(element("/a/1"), "button"), "at1");
        // A path given to a new element of another kind is a new element.
        assert_eq!(known.key_for(element("/a/1"), "check box"), "at3");
        assert_eq!(
            known.get("at3"),
            Some(&(element("/a/1"), "check box".to_owned()))
        );
        assert_eq!(known.get("at9"), None);
        assert_eq!(known.get("ax1"), None);
    }

    fn proc(pid: u32, ppid: u32, name: &str) -> Proc {
        Proc {
            pid,
            ppid,
            name: name.into(),
        }
    }

    #[test]
    fn a_letronna_in_tmux_is_shown_by_every_terminal_with_a_tmux_client() {
        let table = [
            proc(1, 0, "systemd"),
            proc(100, 1, "gnome-terminal-"),
            proc(101, 100, "bash"),
            proc(102, 101, "tmux: client"),
            proc(200, 1, "tmux: server"),
            proc(201, 200, "bash"),
            proc(202, 201, "letronna"),
            proc(300, 1, "kitty"),
            proc(301, 300, "zsh"),
            proc(302, 301, "tmux: client"),
            proc(400, 1, "firefox"),
            proc(500, 1, "screen"),
            proc(501, 500, "bash"),
        ];
        let mut hosts = hosts_of(202, &table);
        hosts.sort_unstable();
        assert_eq!(hosts, [100, 101, 102, 200, 201, 300, 301, 302]);
        // Older tmux and the other multiplexers keep the program's name.
        let old = [
            proc(10, 1, "xterm"),
            proc(11, 10, "tmux"),
            proc(20, 1, "tmux"),
            proc(21, 20, "letronna"),
        ];
        let mut hosts = hosts_of(21, &old);
        hosts.sort_unstable();
        assert_eq!(hosts, [10, 11, 20]);
        assert_eq!(multiplexer("SCREEN"), Some("screen"));
        assert_eq!(multiplexer("tmuxinator"), None);
        // Without a multiplexer, only its own terminal.
        let plain = [
            proc(1, 0, "systemd"),
            proc(10, 1, "xterm"),
            proc(11, 10, "bash"),
            proc(12, 11, "letronna"),
        ];
        assert_eq!(hosts_of(12, &plain), [11, 10]);
        // A loop in the table ends the walk.
        let looped = [proc(5, 6, "a"), proc(6, 5, "b")];
        assert_eq!(hosts_of(5, &looped), [6]);
    }

    #[test]
    fn a_process_line_is_read_whatever_its_name_holds() {
        let line = "4242 (Web Content (x)) S 4200 4242 4200 0 -1 4194560";
        assert_eq!(parse_stat(line), Some(proc(4242, 4200, "Web Content (x)")));
        assert_eq!(parse_stat("garbage"), None);
    }

    #[test]
    fn keys_and_characters_are_the_keysyms_x_knows() {
        assert_eq!(keysym_of("enter"), Some(0xff0d));
        assert_eq!(keysym_of("f1"), Some(0xffbe));
        assert_eq!(keysym_of("f12"), Some(0xffc9));
        assert_eq!(keysym_of("pagedown"), Some(0xff56));
        assert_eq!(keysym_of("s"), Some(0x73));
        assert_eq!(keysym_of("f13"), None);
        assert_eq!(char_sym('é'), 0xe9);
        assert_eq!(char_sym('€'), 0x0100_20ac);
        assert_eq!(char_sym('\n'), RETURN);
        assert_eq!(char_sym('\t'), TAB);
    }

    #[test]
    fn the_desktops_own_shortcuts_are_never_pressed() {
        use Modifier::{Alt, Command, Control, Shift};
        for (key, held) in [
            ("backspace", &[Control, Alt][..]),
            ("delete", &[Control, Alt]),
            ("f3", &[Control, Alt]),
            ("t", &[Control, Alt]),
            ("t", &[Control, Alt, Shift]),
            ("l", &[Command]),
            ("l", &[Control, Alt]),
            ("f2", &[Alt]),
        ] {
            assert!(system_shortcut(key, held).is_some(), "{key} {held:?}");
        }
        for (key, held) in [
            ("t", &[Control][..]),
            ("f2", &[]),
            ("l", &[Control]),
            ("delete", &[Control]),
            ("f4", &[Alt]),
        ] {
            assert_eq!(system_shortcut(key, held), None, "{key} {held:?}");
        }
    }

    #[test]
    fn nothing_is_sent_unless_the_app_is_in_front() {
        let app = App {
            id: "gedit".into(),
            name: "Gedit".into(),
            pids: vec![40],
        };
        assert_eq!(in_front(&app, true), Ok(()));
        let error = in_front(&app, false).unwrap_err();
        assert!(error.contains("Gedit is not in front"), "{error}");
        assert_eq!(
            centre(Some(rect(10.0, 20.0, 30.0, 40.0))),
            Ok(Point { x: 25.0, y: 40.0 })
        );
        assert!(centre(Some(rect(10.0, 20.0, 0.0, 40.0))).is_err());
    }
}
