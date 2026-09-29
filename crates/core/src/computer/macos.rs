//! Computer use on macOS: apps and windows from the window server, their
//! elements through the Accessibility API, pictures through Core Graphics.
//!
//! An element is pressed, focused or given its text through the
//! Accessibility API wherever it takes that, since that reaches the element
//! itself whatever is in front. Made-up input is the fallback, and before
//! each event the app is checked to be in front, and a click or a scroll to
//! land on one of its windows; keys go to its process alone, so a window
//! that comes up in between is never typed into. The Accessibility and
//! Screen Recording permissions are only looked at, never asked for: a
//! missing one is named in the refusal, with where to turn it on.
//!
//! The window server lists windows without any permission, so the app list
//! and the look at what is under a spot, which the gate makes on the
//! window's UI thread, cost one listing and no message to another app.
// Tests use a fake backend, so outside the pure parts this is not reached.
#![cfg_attr(test, allow(dead_code))]

mod ax;
mod sys;

use super::{Act, App, Backend, Bounds, Direction, Element, Modifier, Point, Shot, Spot, lock};
use ax::AxElement;
use core_foundation::base::TCFType;
use core_foundation::string::CFString;
use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex, Once, OnceLock};
use std::time::{Duration, Instant};

/// How long a listing of the apps is used again: the gate and the call it
/// lets through list them moments apart, and a new window is seen soon.
const APPS_FRESH: Duration = Duration::from_millis(500);
/// How long the processes showing this one are used again; a new tmux
/// client is seen within it.
const HOSTS_FRESH: Duration = Duration::from_secs(2);
/// How long one message to another app waits for its answer, in seconds,
/// down from the system's six, so a hung app stalls a call only briefly.
const APP_ANSWERS: f32 = 1.5;
/// How long one listing of an app's elements may take before it stops.
const WALK_TIME: Duration = Duration::from_secs(4);
/// The most elements one listing reads: enough for a busy window, where a
/// web page's full tree would take far longer than the rest of the call.
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
/// How often the app in front is looked at while waiting for it.
const FRONT_POLL: Duration = Duration::from_millis(50);
/// The pause after each made-up event, which apps need to take them in turn.
const EVENT_GAP: Duration = Duration::from_millis(20);
/// The most UTF-16 units one key event carries; macOS takes no more.
const TYPED_UNITS: usize = 20;
/// The furthest up the parents a walk goes, far past any real chain.
const ANCESTORS_MOST: usize = 64;
/// Terminal multiplexers, whose server has no terminal of its own: the
/// terminals showing Letronna are those of its clients.
const MULTIPLEXERS: &[&str] = &["tmux", "screen", "zellij"];

/// Roles listed though they have no name or value: what can be pressed,
/// typed into or moved.
const ACTIONABLE: &[&str] = &[
    "AXButton",
    "AXCheckBox",
    "AXColorWell",
    "AXComboBox",
    "AXDateField",
    "AXDisclosureTriangle",
    "AXIncrementor",
    "AXLink",
    "AXMenuBarItem",
    "AXMenuButton",
    "AXMenuItem",
    "AXPopUpButton",
    "AXRadioButton",
    "AXSlider",
    "AXTextArea",
    "AXTextField",
];
/// Subroles that say more than their role, shown in its place.
const SUBROLES: &[&str] = &[
    "AXCloseButton",
    "AXFullScreenButton",
    "AXMinimizeButton",
    "AXSearchField",
    "AXSwitch",
    "AXTabButton",
    "AXToggle",
    "AXZoomButton",
];
/// The attributes one message reads from each menu item: its shortcut's
/// character and modifiers, whether it is enabled, and its title.
const MENU_READ: [&str; 4] = [
    "AXMenuItemCmdChar",
    "AXMenuItemCmdModifiers",
    "AXEnabled",
    "AXTitle",
];
/// The attributes one message reads from each element as a listing walks.
const READ: [&str; 7] = [
    "AXRole",
    "AXSubrole",
    "AXTitle",
    "AXDescription",
    "AXValue",
    "AXPosition",
    "AXSize",
];

/// One window on screen, as the window server lists it.
#[derive(Clone, Debug, PartialEq)]
struct Window {
    id: u32,
    pid: u32,
    /// The name of the app whose window it is.
    owner: String,
    /// Normal windows are 0; menus, panels, the Dock and the menu bar above.
    layer: i64,
    bounds: Bounds,
    alpha: f64,
}

/// One process, as the process table has it.
#[derive(Clone, Debug, PartialEq)]
struct Proc {
    pid: u32,
    ppid: u32,
    name: String,
}

/// The macOS backend, one for the program, so what it has seen is shared.
pub(super) fn backend() -> Option<Arc<dyn Backend>> {
    static MAC: OnceLock<Arc<Mac>> = OnceLock::new();
    let mac: Arc<dyn Backend> = MAC.get_or_init(|| Arc::new(Mac::default())).clone();
    Some(mac)
}

#[derive(Default)]
struct Mac {
    apps: Mutex<Option<(Instant, Vec<App>)>>,
    hosts: Mutex<Option<(Instant, Vec<u32>)>>,
    known: Mutex<Known<AxElement>>,
    /// Held while reading elements or acting, so that two sessions' events
    /// never interleave.
    busy: Mutex<()>,
}

impl Backend for Mac {
    fn apps(&self) -> Result<Vec<App>, String> {
        let mut cached = lock(&self.apps);
        if let Some((at, apps)) = cached.as_ref()
            && at.elapsed() < APPS_FRESH
        {
            return Ok(apps.clone());
        }
        let apps = apps_of(&sys::windows()?, identity);
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
        let hosts = hosts_of(std::process::id(), &sys::processes());
        *cached = Some((Instant::now(), hosts.clone()));
        hosts
    }

    fn screenshot(&self, app: Option<&App>) -> Result<Shot, String> {
        if !sys::may_capture() {
            return Err(missing(Permission::ScreenRecording, &grantee_here()));
        }
        let (area, ids) = match app {
            None => (
                sys::screen().ok_or("macOS did not say where the screen is.")?,
                None,
            ),
            Some(app) => {
                let windows: Vec<Window> = sys::windows()?
                    .into_iter()
                    .filter(|window| app.pids.contains(&window.pid) && visible(window))
                    .collect();
                let area = union(windows.iter().map(|window| window.bounds))
                    .ok_or_else(|| format!("{} has no window on screen.", app.name))?;
                (
                    area,
                    Some(windows.iter().map(|window| window.id).collect::<Vec<_>>()),
                )
            }
        };
        Ok(Shot {
            picture: sys::capture(area, ids.as_deref())?,
            at: Point {
                x: area.x,
                y: area.y,
            },
        })
    }

    fn elements(&self, app: &App) -> Result<Vec<Element>, String> {
        let _busy = lock(&self.busy);
        allowed()?;
        let (window, view) = front_window(app)?;
        let deadline = Instant::now() + WALK_TIME;
        let found = walk(window, view, read, children, || Instant::now() < deadline);
        let mut known = lock(&self.known);
        Ok(found
            .into_iter()
            .map(|(handle, mut element)| {
                let hash = handle.hash();
                element.key = known.key_for(handle, hash);
                element
            })
            .collect())
    }

    fn app_at(&self, point: Point) -> Result<Option<App>, String> {
        let windows = sys::windows()?;
        let Some(window) = window_at(&windows, point) else {
            return Ok(None);
        };
        if let Some(app) = self
            .apps()?
            .into_iter()
            .find(|app| app.pids.contains(&window.pid))
        {
            return Ok(Some(app));
        }
        // A window no listed app has: the menu bar, the Dock, a system panel.
        let id = identity(window.pid, &window.owner);
        let name = Some(window.owner.clone()).filter(|owner| !owner.is_empty());
        Ok(Some(App {
            name: name.unwrap_or_else(|| id.clone()),
            id,
            pids: vec![window.pid],
        }))
    }

    fn act(&self, app: &App, act: &Act) -> Result<(), String> {
        let _busy = lock(&self.busy);
        allowed()?;
        match act {
            Act::Click(Spot::Element(key)) => {
                let element = self.element(app, key)?;
                if element.actions().iter().any(|action| action == "AXPress") {
                    return element
                        .perform("AXPress")
                        .map_err(|code| format!("Could not press it: {}.", ax::why(code)));
                }
                raise(&element);
                click(app, centre(&element)?)
            }
            Act::Click(Spot::Point(at)) => click(app, *at),
            Act::Type { text, into } => {
                let element = match into {
                    Some(Spot::Element(key)) => Some(self.element(app, key)?),
                    Some(Spot::Point(at)) => {
                        click(app, *at)?;
                        None
                    }
                    None => None,
                };
                let target = element.clone().or_else(|| focused_element(app));
                if let Some(target) = &target
                    && insert(target, text)
                {
                    return Ok(());
                }
                // Keys go to what has the keyboard, so the element takes it
                // first, by the Accessibility API or else by a click.
                if let Some(element) = &element
                    && element.set_flag("AXFocused", true).is_err()
                {
                    raise(element);
                    click(app, centre(element)?)?;
                }
                type_keys(app, text)
            }
            Act::Key { key, held } => press(app, key, held),
            Act::Scroll {
                at,
                direction,
                lines,
            } => {
                let at = match at {
                    Some(Spot::Element(key)) => centre(&self.element(app, key)?)?,
                    Some(Spot::Point(at)) => *at,
                    None => middle(front_window(app)?.1),
                };
                scroll(app, at, *direction, *lines)
            }
        }
    }
}

impl Mac {
    /// The element listed under `key`, if it is still `app`'s.
    fn element(&self, app: &App, key: &str) -> Result<AxElement, String> {
        let element = lock(&self.known).get(key).cloned();
        let element =
            element.ok_or("That element is no longer known: list the app's elements again.")?;
        match element.pid() {
            Some(pid) if app.pids.contains(&pid) => Ok(element),
            Some(_) => Err(format!("That element is not one of {}'s.", app.name)),
            None => {
                Err("That element is no longer on screen: list the app's elements again.".into())
            }
        }
    }
}

/// Refuses unless macOS lets this process read and use other apps.
fn allowed() -> Result<(), String> {
    if !ax::trusted() {
        return Err(missing(Permission::Accessibility, &grantee_here()));
    }
    static TIMEOUT: Once = Once::new();
    TIMEOUT.call_once(|| {
        if let Some(system) = AxElement::system() {
            system.set_timeout(APP_ANSWERS);
        }
    });
    Ok(())
}

/// The process whose app has the keyboard, as the system says now.
fn focused_pid() -> Option<u32> {
    AxElement::system()?.element("AXFocusedApplication")?.pid()
}

/// Brings `app` to the front, waiting until it is there.
fn to_front(app: &App) -> Result<(), String> {
    if in_front(app, focused_pid()).is_ok() {
        return Ok(());
    }
    if let Some(element) = app.pids.first().and_then(|&pid| AxElement::app(pid)) {
        // Refused or not, the wait below says whether it came.
        let _ = element.set_flag("AXFrontmost", true);
    }
    let deadline = Instant::now() + FRONT_WAIT;
    while Instant::now() < deadline {
        std::thread::sleep(FRONT_POLL);
        if in_front(app, focused_pid()).is_ok() {
            return Ok(());
        }
    }
    Err(format!(
        "{} did not come to the front, so nothing was sent to it.",
        app.name
    ))
}

/// Refuses unless `app` is in front and the window on top at `at` is one of
/// its own, as the system says right now.
fn still_there(app: &App, at: Option<Point>) -> Result<u32, String> {
    if !sys::may_post() {
        return Err(missing(Permission::Accessibility, &grantee_here()));
    }
    let pid = in_front(app, focused_pid())?;
    if let Some(at) = at {
        over(&sys::windows()?, at, app)?;
    }
    Ok(pid)
}

/// Sends `down`, then `up`, each checked first; an `up` goes even when its
/// check fails, so no button or key is left held, since it reaches only
/// what took the `down`.
fn pair(
    app: &App,
    at: Option<Point>,
    down: impl FnOnce(u32) -> Result<(), String>,
    up: impl FnOnce(u32) -> Result<(), String>,
) -> Result<(), String> {
    let pid = still_there(app, at)?;
    down(pid)?;
    std::thread::sleep(EVENT_GAP);
    let checked = still_there(app, at);
    up(pid)?;
    std::thread::sleep(EVENT_GAP);
    checked.map(|_| ())
}

/// Clicks at `at` with made-up input, `app` in front and its window there.
fn click(app: &App, at: Point) -> Result<(), String> {
    to_front(app)?;
    still_there(app, Some(at))?;
    sys::mouse(sys::Mouse::Move, at)?;
    std::thread::sleep(EVENT_GAP);
    pair(
        app,
        Some(at),
        |_| sys::mouse(sys::Mouse::Down, at),
        |_| sys::mouse(sys::Mouse::Up, at),
    )
}

/// Types `text` with made-up keys, to `app`'s process in front.
fn type_keys(app: &App, text: &str) -> Result<(), String> {
    to_front(app)?;
    for piece in pieces(text) {
        match piece {
            Piece::Text(units) => pair(
                app,
                None,
                |pid| sys::text(pid, &units, true),
                |pid| sys::text(pid, &units, false),
            )?,
            Piece::Key(code) => pair(
                app,
                None,
                |pid| sys::key(pid, code, 0, "", true),
                |pid| sys::key(pid, code, 0, "", false),
            )?,
        }
    }
    Ok(())
}

/// Presses `key` with `held` down in `app`: a shortcut through the app's
/// menu item for it where it has one, since a menu knows its shortcut by
/// character, whatever the keyboard; else as keys to its process in front,
/// a character with no modifier typed as itself.
fn press(app: &App, key: &str, held: &[Modifier]) -> Result<(), String> {
    if let Some(what) = system_shortcut(key, held) {
        return Err(format!(
            "Refused: that shortcut {what}, which acts on the whole Mac rather \
             than {}, so Letronna never presses it.",
            app.name
        ));
    }
    let character = Some(key).filter(|key| key.chars().count() == 1);
    if character.is_some() && held.is_empty() {
        return type_keys(app, key);
    }
    if let Some(character) = character
        && let Some((item, title, enabled)) = menu_item(app, character, held)
    {
        if !enabled {
            return Err(format!(
                "Nothing was done: that shortcut is {}'s menu item \"{title}\", \
                 which is greyed out now.",
                app.name
            ));
        }
        to_front(app)?;
        return item
            .perform("AXPress")
            .map_err(|code| format!("Could not choose \"{title}\": {}.", ax::why(code)));
    }
    let Some(code) = keycode(key) else {
        return Err(format!(
            "`{key}` cannot be pressed with modifiers here: only the letters, digits \
             and marks of a US keyboard can, or a shortcut in the app's menus."
        ));
    };
    let flags = flags_of(held);
    let typed = character.unwrap_or_default();
    to_front(app)?;
    pair(
        app,
        None,
        |pid| sys::key(pid, code, flags, typed, true),
        |pid| sys::key(pid, code, flags, typed, false),
    )
}

/// The item in `app`'s menus whose shortcut is `character` with `held`, its
/// title, and whether it can be chosen now. The Apple menu, which every
/// app's menu bar holds and whose items act on the whole Mac, is left out.
fn menu_item(app: &App, character: &str, held: &[Modifier]) -> Option<(AxElement, String, bool)> {
    let bar = AxElement::app(*app.pids.first()?)?.element("AXMenuBar")?;
    let mut stack: Vec<AxElement> = bar.elements("AXChildren")?.into_iter().skip(1).collect();
    let wanted = menu_modifiers(held);
    let deadline = Instant::now() + WALK_TIME;
    let mut reads = 0;
    while let Some(item) = stack.pop() {
        reads += 1;
        if reads > READ_MOST || Instant::now() > deadline {
            return None;
        }
        let Ok(values) = item.get_many(&MENU_READ) else {
            continue;
        };
        let value = |at: usize| values.get(at).and_then(Option::as_ref);
        let words = |at: usize| value(at).and_then(ax::text).unwrap_or_default();
        let modifiers = words(1).parse::<i64>().unwrap_or(0);
        if !words(0).is_empty() && words(0).eq_ignore_ascii_case(character) && modifiers == wanted {
            return Some((item, words(3), words(2) != "off"));
        }
        stack.extend(item.elements("AXChildren").unwrap_or_default());
    }
    None
}

/// Scrolls `lines` towards `direction` at `at`, `app` in front and its window
/// there.
fn scroll(app: &App, at: Point, direction: Direction, lines: u32) -> Result<(), String> {
    let lines = lines.min(i32::MAX as u32) as i32;
    let (vertical, horizontal) = match direction {
        Direction::Up => (lines, 0),
        Direction::Down => (-lines, 0),
        Direction::Left => (0, lines),
        Direction::Right => (0, -lines),
    };
    to_front(app)?;
    still_there(app, Some(at))?;
    sys::mouse(sys::Mouse::Move, at)?;
    std::thread::sleep(EVENT_GAP);
    still_there(app, Some(at))?;
    sys::scroll(vertical, horizontal)
}

/// Puts `text` in `element` where its cursor is, as typing does, through the
/// Accessibility API; false where the element does not take text that way,
/// or where `text` holds a return or a tab, which typing presses as keys.
fn insert(element: &AxElement, text: &str) -> bool {
    if text.contains(['\n', '\r', '\t']) || !element.settable("AXSelectedText") {
        return false;
    }
    if element.settable("AXFocused") {
        let _ = element.set_flag("AXFocused", true);
    }
    let text = CFString::new(text).as_CFType();
    element.set("AXSelectedText", &text).is_ok()
}

/// Brings `element`'s window above the app's others.
fn raise(element: &AxElement) {
    if let Some(window) = element.element("AXWindow") {
        let _ = window.perform("AXRaise");
    }
}

/// Where `element` is on screen.
fn frame_of(element: &AxElement) -> Option<Bounds> {
    let values = element.get_many(&["AXPosition", "AXSize"]).ok()?;
    let at = values.first()?.as_ref().and_then(ax::point)?;
    let size = values.get(1)?.as_ref().and_then(ax::size)?;
    Some(Bounds {
        x: at.x,
        y: at.y,
        width: size.width,
        height: size.height,
    })
}

/// The middle of `element`, where a click on it lands.
fn centre(element: &AxElement) -> Result<Point, String> {
    frame_of(element)
        .filter(|frame| frame.width > 0.0 && frame.height > 0.0)
        .map(middle)
        .ok_or_else(|| "That element has no place on screen to click.".into())
}

fn middle(bounds: Bounds) -> Point {
    Point {
        x: bounds.x + bounds.width / 2.0,
        y: bounds.y + bounds.height / 2.0,
    }
}

/// The element of `app`'s that has its keyboard focus.
fn focused_element(app: &App) -> Option<AxElement> {
    app.pids
        .iter()
        .find_map(|&pid| AxElement::app(pid)?.element("AXFocusedUIElement"))
}

/// `app`'s window in front, and where it is.
fn front_window(app: &App) -> Result<(AxElement, Bounds), String> {
    for &pid in &app.pids {
        let Some(element) = AxElement::app(pid) else {
            continue;
        };
        let window = element
            .element("AXFocusedWindow")
            .or_else(|| element.element("AXMainWindow"))
            .or_else(|| element.elements("AXWindows")?.into_iter().next());
        if let Some(window) = window
            && let Some(frame) = frame_of(&window)
        {
            return Ok((window, frame));
        }
    }
    Err(format!(
        "{} shows no window the Accessibility API can read.",
        app.name
    ))
}

/// What `element` says about itself, in one message to its app.
fn read(element: &AxElement) -> Option<Node> {
    let values = element.get_many(&READ).ok()?;
    let value = |at: usize| values.get(at).and_then(Option::as_ref);
    let words = |at: usize| value(at).and_then(ax::text).unwrap_or_default();
    let frame = match (value(5).and_then(ax::point), value(6).and_then(ax::size)) {
        (Some(at), Some(size)) => Some(Bounds {
            x: at.x,
            y: at.y,
            width: size.width,
            height: size.height,
        }),
        _ => None,
    };
    Some(Node {
        role: words(0),
        subrole: words(1),
        title: words(2),
        description: words(3),
        value: value(4).and_then(ax::text),
        frame,
    })
}

/// The children a listing walks: for tables and lists, which can hold
/// thousands, only the rows or items in view.
fn children(element: &AxElement, node: &Node) -> Vec<AxElement> {
    let in_view = match node.role.as_str() {
        "AXTable" | "AXOutline" => Some("AXVisibleRows"),
        "AXList" | "AXBrowser" | "AXGrid" => Some("AXVisibleChildren"),
        _ => None,
    };
    in_view
        .and_then(|attribute| element.elements(attribute))
        .or_else(|| element.elements("AXChildren"))
        .unwrap_or_default()
}

/// The id grants and rules know process `pid` by: its app bundle's id, or
/// its program's name outside a bundle, as on the other systems; the name
/// its window shows only where the process cannot be looked at.
fn identity(pid: u32, owner: &str) -> String {
    let Some(path) = sys::path_of(pid) else {
        return Some(owner.to_owned())
            .filter(|owner| !owner.is_empty())
            .unwrap_or_else(|| format!("pid {pid}"));
    };
    bundle_of(&path)
        .and_then(sys::bundle_id)
        .unwrap_or_else(|| program(&path).to_owned())
}

/// Who macOS counts this process's permissions under, looked up now.
fn grantee_here() -> String {
    let procs = sys::processes();
    let own = std::process::id();
    let by_pid: HashMap<u32, &Proc> = procs.iter().map(|proc| (proc.pid, proc)).collect();
    let chain: Vec<(String, String)> = ancestors(own, &by_pid)
        .into_iter()
        .filter_map(|pid| Some((by_pid.get(&pid)?.name.clone(), sys::path_of(pid)?)))
        .collect();
    grantee(sys::path_of(own).as_deref(), &chain)
}

/// What one element says about itself.
#[derive(Clone, Debug, Default, PartialEq)]
struct Node {
    role: String,
    subrole: String,
    title: String,
    description: String,
    value: Option<String>,
    frame: Option<Bounds>,
}

/// The tree under `root` flattened in the order a screen reader meets it:
/// what has a name, a value or an action, and can be seen within `view` and
/// the scroll areas it sits in. What is out of view is skipped with all it
/// holds, and the walk stops at `LISTED_MOST` found, `READ_MOST` read,
/// `DEPTH_MOST` deep, or when `in_time` says so.
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
        // A container can report no size while its children have one.
        let frame = node.frame.filter(|f| f.width > 0.0 && f.height > 0.0);
        if frame.is_some_and(|frame| !overlaps(frame, view)) {
            continue;
        }
        let inner = match frame {
            Some(frame) if node.role == "AXScrollArea" => intersection(frame, view),
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
    let name = [&node.title, &node.description]
        .into_iter()
        .map(|text| text.trim())
        .find(|text| !text.is_empty())
        .unwrap_or_default();
    let value = node
        .value
        .as_deref()
        .filter(|_| node.subrole != "AXSecureTextField")
        .filter(|value| !value.trim().is_empty())
        .map(|value| value.chars().take(TEXT_KEPT).collect::<String>());
    if name.is_empty() && value.is_none() && !ACTIONABLE.contains(&node.role.as_str()) {
        return None;
    }
    Some(Element {
        key: String::new(),
        role: role_name(&node.role, &node.subrole),
        name: name.chars().take(TEXT_KEPT).collect(),
        value,
        bounds: frame,
    })
}

/// A role in plain words: `AXPopUpButton` is "pop up button", a password
/// field says so, and a subrole that says more is shown instead.
fn role_name(role: &str, subrole: &str) -> String {
    if subrole == "AXSecureTextField" {
        return "password field".into();
    }
    if role == "AXStaticText" {
        return "text".into();
    }
    let shown = if SUBROLES.contains(&subrole) {
        subrole
    } else {
        role
    };
    let shown: Vec<char> = shown.strip_prefix("AX").unwrap_or(shown).chars().collect();
    let mut words = String::new();
    for (at, &c) in shown.iter().enumerate() {
        let after_lower = at > 0 && shown[at - 1].is_lowercase();
        let ends_capitals = at > 0
            && shown[at - 1].is_uppercase()
            && shown.get(at + 1).is_some_and(|next| next.is_lowercase());
        if c.is_uppercase() && (after_lower || ends_capitals) {
            words.push(' ');
        }
        words.extend(c.to_lowercase());
    }
    if words.is_empty() {
        return "element".into();
    }
    words
}

/// The elements listed so far, under keys that stay theirs: an element met
/// again, which the system says is equal, gets its old key, and a key is
/// never given to another element, so a stale one finds nothing.
struct Known<E> {
    next: u64,
    /// Keys by a hash equal elements share, to find them again quickly.
    by_hash: HashMap<u64, Vec<u64>>,
    by_key: BTreeMap<u64, E>,
}

impl<E> Default for Known<E> {
    fn default() -> Self {
        Known {
            next: 0,
            by_hash: HashMap::new(),
            by_key: BTreeMap::new(),
        }
    }
}

impl<E: PartialEq> Known<E> {
    /// `element`'s key, a new one the first time it is met.
    fn key_for(&mut self, element: E, hash: u64) -> String {
        let known = self.by_hash.get(&hash).and_then(|keys| {
            keys.iter()
                .find(|key| self.by_key.get(key) == Some(&element))
                .copied()
        });
        if let Some(key) = known {
            return format!("ax{key}");
        }
        if self.by_key.len() >= KNOWN_KEPT {
            // The oldest go first: the model has long moved on from them.
            let kept_from = self.next.saturating_sub(KNOWN_KEPT as u64 / 2);
            self.by_key = self.by_key.split_off(&kept_from);
            let kept = &self.by_key;
            self.by_hash.retain(|_, keys| {
                keys.retain(|key| kept.contains_key(key));
                !keys.is_empty()
            });
        }
        self.next += 1;
        self.by_hash.entry(hash).or_default().push(self.next);
        self.by_key.insert(self.next, element);
        format!("ax{}", self.next)
    }

    /// The element listed under `key`, if it is still kept.
    fn get(&self, key: &str) -> Option<&E> {
        let number = key.strip_prefix("ax")?.parse().ok()?;
        self.by_key.get(&number)
    }
}

/// The apps with a normal window on screen, the one whose window is in
/// front first, an app's processes together under the id `identify` gives.
fn apps_of(windows: &[Window], mut identify: impl FnMut(u32, &str) -> String) -> Vec<App> {
    let mut apps: Vec<App> = Vec::new();
    let mut seen = Vec::new();
    for window in windows
        .iter()
        .filter(|window| window.layer == 0 && visible(window))
    {
        if seen.contains(&window.pid) {
            continue;
        }
        seen.push(window.pid);
        let id = identify(window.pid, &window.owner);
        match apps.iter_mut().find(|app| app.id.eq_ignore_ascii_case(&id)) {
            Some(app) => app.pids.push(window.pid),
            None => apps.push(App {
                name: Some(window.owner.clone())
                    .filter(|owner| !owner.is_empty())
                    .unwrap_or_else(|| id.clone()),
                id,
                pids: vec![window.pid],
            }),
        }
    }
    apps
}

/// Whether `window` can be seen: not see-through, and more than a sliver.
fn visible(window: &Window) -> bool {
    window.alpha > 0.0 && window.bounds.width >= 2.0 && window.bounds.height >= 2.0
}

/// The window on top at `point`, of any layer, a menu or the menu bar
/// included; see-through ones are passed over, as clicks pass through them.
fn window_at(windows: &[Window], point: Point) -> Option<&Window> {
    windows
        .iter()
        .find(|window| window.alpha > 0.0 && contains(window.bounds, point))
}

fn contains(bounds: Bounds, point: Point) -> bool {
    point.x >= bounds.x
        && point.y >= bounds.y
        && point.x < bounds.x + bounds.width
        && point.y < bounds.y + bounds.height
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

/// Refuses unless the window on top at `at` is one of `app`'s.
fn over(windows: &[Window], at: Point, app: &App) -> Result<(), String> {
    let (x, y) = (at.x.round() as i64, at.y.round() as i64);
    match window_at(windows, at) {
        Some(window) if app.pids.contains(&window.pid) => Ok(()),
        Some(window) => Err(format!(
            "Stopped: {}'s window is on top at {x},{y}, not {}'s, so nothing more \
             was sent there. Take a screenshot to see what is.",
            window.owner, app.name
        )),
        None => Err(format!(
            "Stopped: no window of {}'s is at {x},{y}, so nothing more was sent there.",
            app.name
        )),
    }
}

/// `app`'s process that has the keyboard, given the one that has it,
/// `focused`, or why nothing may be sent.
fn in_front(app: &App, focused: Option<u32>) -> Result<u32, String> {
    match focused {
        Some(pid) if app.pids.contains(&pid) => Ok(pid),
        _ => Err(format!(
            "Stopped: {} is not in front, so nothing more was sent. Take a \
             screenshot to see what is.",
            app.name
        )),
    }
}

/// The processes whose windows show `own`: its parents up to the terminal
/// app, and where one is a terminal multiplexer such as tmux, whose server
/// has no terminal, the parents of each of its clients, in whichever
/// terminal they run. Every client of every server of that kind counts: a
/// terminal left out would be one Letronna could type into.
fn hosts_of(own: u32, procs: &[Proc]) -> Vec<u32> {
    let by_pid: HashMap<u32, &Proc> = procs.iter().map(|proc| (proc.pid, proc)).collect();
    let mut hosts = ancestors(own, &by_pid);
    let multiplexers: Vec<String> = hosts
        .iter()
        .filter_map(|pid| by_pid.get(pid))
        .map(|proc| proc.name.to_lowercase())
        .filter(|name| MULTIPLEXERS.contains(&name.as_str()))
        .collect();
    for proc in procs {
        let client = multiplexers.contains(&proc.name.to_lowercase());
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

/// `pid`'s parents, nearest first, up to but leaving out launchd.
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

/// The app bundle a program at `path` is part of: the innermost, since an
/// app's helpers are apps of their own.
fn bundle_of(path: &str) -> Option<&str> {
    let end = path.rfind(".app/Contents/")?;
    Some(&path[..end + ".app".len()])
}

/// The program's file name at the end of `path`.
fn program(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

/// Who System Settings lists this process's permissions under: the app
/// bundle it runs from; else a terminal multiplexer among its parents, as
/// the lists may show it; else the terminal app among its parents. `chain`
/// is its parents' names and paths, nearest first.
fn grantee(own: Option<&str>, chain: &[(String, String)]) -> String {
    let app_name = |path: &str| {
        let bundle = bundle_of(path)?;
        Some(program(bundle).trim_end_matches(".app").to_owned())
    };
    if let Some(name) = own.and_then(app_name) {
        return name;
    }
    if let Some((name, _)) = chain
        .iter()
        .find(|(name, _)| MULTIPLEXERS.contains(&name.to_lowercase().as_str()))
    {
        return format!("{name}, or the terminal app {name} was started from");
    }
    chain
        .iter()
        .find_map(|(_, path)| app_name(path))
        .unwrap_or_else(|| "the terminal app Letronna runs in".into())
}

/// A permission computer use needs from macOS.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Permission {
    Accessibility,
    ScreenRecording,
}

/// Why a call cannot run without `permission`, and where the user turns it
/// on for `grantee`. Letronna never asks for it itself: asking pops a
/// system prompt over whatever the user is doing.
fn missing(permission: Permission, grantee: &str) -> String {
    let (what, needs, pane, then) = match permission {
        Permission::Accessibility => (
            "Accessibility",
            "reading and using other apps",
            "Accessibility",
            "",
        ),
        Permission::ScreenRecording => (
            "Screen Recording",
            "screenshots",
            "Screen & System Audio Recording",
            ", then quit and reopen it",
        ),
    };
    format!(
        "macOS has not given Letronna the {what} permission, which {needs} needs. \
         The user can turn it on in System Settings > Privacy & Security > {pane}, \
         for {grantee}{then}. Carry on without it until then."
    )
}

/// The key code of `key`, a name from the tool's list or one character, at
/// its place on a US keyboard; `delete` is the forward delete, as on the
/// other systems, and `backspace` the key macOS calls delete.
fn keycode(key: &str) -> Option<u16> {
    let code = match key {
        "enter" | "return" => 0x24,
        "tab" => 0x30,
        "space" | " " => 0x31,
        "backspace" => 0x33,
        "escape" => 0x35,
        "delete" => 0x75,
        "home" => 0x73,
        "end" => 0x77,
        "pageup" => 0x74,
        "pagedown" => 0x79,
        "left" => 0x7B,
        "right" => 0x7C,
        "down" => 0x7D,
        "up" => 0x7E,
        "f1" => 0x7A,
        "f2" => 0x78,
        "f3" => 0x63,
        "f4" => 0x76,
        "f5" => 0x60,
        "f6" => 0x61,
        "f7" => 0x62,
        "f8" => 0x64,
        "f9" => 0x65,
        "f10" => 0x6D,
        "f11" => 0x67,
        "f12" => 0x6F,
        _ => {
            let mut chars = key.chars();
            let (Some(c), None) = (chars.next(), chars.next()) else {
                return None;
            };
            let c = c.to_ascii_lowercase();
            return US_KEYS
                .iter()
                .find(|(known, _)| *known == c)
                .map(|(_, code)| *code);
        }
    };
    Some(code)
}

/// The characters of a US keyboard's keys, unshifted, with their codes.
const US_KEYS: [(char, u16); 47] = [
    ('a', 0x00),
    ('s', 0x01),
    ('d', 0x02),
    ('f', 0x03),
    ('h', 0x04),
    ('g', 0x05),
    ('z', 0x06),
    ('x', 0x07),
    ('c', 0x08),
    ('v', 0x09),
    ('b', 0x0B),
    ('q', 0x0C),
    ('w', 0x0D),
    ('e', 0x0E),
    ('r', 0x0F),
    ('y', 0x10),
    ('t', 0x11),
    ('1', 0x12),
    ('2', 0x13),
    ('3', 0x14),
    ('4', 0x15),
    ('6', 0x16),
    ('5', 0x17),
    ('=', 0x18),
    ('9', 0x19),
    ('7', 0x1A),
    ('-', 0x1B),
    ('8', 0x1C),
    ('0', 0x1D),
    (']', 0x1E),
    ('o', 0x1F),
    ('u', 0x20),
    ('[', 0x21),
    ('i', 0x22),
    ('p', 0x23),
    ('l', 0x25),
    ('j', 0x26),
    ('\'', 0x27),
    ('k', 0x28),
    (';', 0x29),
    ('\\', 0x2A),
    (',', 0x2B),
    ('/', 0x2C),
    ('n', 0x2D),
    ('m', 0x2E),
    ('.', 0x2F),
    ('`', 0x32),
];

/// How a menu item's `AXMenuItemCmdModifiers` says `held`: Command is
/// assumed, so its absence is the flag.
fn menu_modifiers(held: &[Modifier]) -> i64 {
    let flag = |modifier: Modifier, bit: i64| if held.contains(&modifier) { bit } else { 0 };
    flag(Modifier::Shift, 1)
        | flag(Modifier::Alt, 2)
        | flag(Modifier::Control, 4)
        | if held.contains(&Modifier::Command) {
            0
        } else {
            8
        }
}

/// What `key` with `held` does to the whole Mac, if it is one of its own
/// shortcuts: the Apple menu's log out and lock, which reach it through any
/// app's menu bar, and force quit.
fn system_shortcut(key: &str, held: &[Modifier]) -> Option<&'static str> {
    use Modifier::{Alt, Command, Control, Shift};
    let has = |wanted: &[Modifier]| {
        wanted.iter().all(|modifier| held.contains(modifier))
            && held.iter().all(|modifier| wanted.contains(modifier))
    };
    match key {
        "q" if has(&[Command, Shift]) || has(&[Command, Shift, Alt]) => Some("logs out"),
        "q" if has(&[Command, Control]) => Some("locks the screen"),
        "escape" if has(&[Command, Alt]) => Some("opens Force Quit"),
        _ => None,
    }
}

/// The event flags for `held`.
fn flags_of(held: &[Modifier]) -> u64 {
    use core_graphics::event::CGEventFlags;
    held.iter()
        .map(|modifier| match modifier {
            Modifier::Command => CGEventFlags::CGEventFlagCommand,
            Modifier::Control => CGEventFlags::CGEventFlagControl,
            Modifier::Alt => CGEventFlags::CGEventFlagAlternate,
            Modifier::Shift => CGEventFlags::CGEventFlagShift,
        })
        .fold(0, |flags, flag| flags | flag.bits())
}

/// A part of typed text: a run of characters, or a key such as return.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Piece {
    Text(Vec<u16>),
    Key(u16),
}

/// `text` as key events: runs of at most `TYPED_UNITS`, never splitting a
/// character, with a return or a tab pressed as its key.
fn pieces(text: &str) -> Vec<Piece> {
    let mut found = Vec::new();
    let mut run: Vec<u16> = Vec::new();
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        let key = match c {
            '\r' => {
                chars.next_if_eq(&'\n');
                Some(0x24)
            }
            '\n' => Some(0x24),
            '\t' => Some(0x30),
            _ => None,
        };
        let mut units = [0; 2];
        let units = c.encode_utf16(&mut units);
        if (key.is_some() || run.len() + units.len() > TYPED_UNITS) && !run.is_empty() {
            found.push(Piece::Text(std::mem::take(&mut run)));
        }
        match key {
            Some(code) => found.push(Piece::Key(code)),
            None => run.extend_from_slice(units),
        }
    }
    if !run.is_empty() {
        found.push(Piece::Text(run));
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

    fn node(role: &str, title: &str, value: Option<&str>, frame: Option<Bounds>) -> Node {
        Node {
            role: role.into(),
            title: title.into(),
            value: value.map(str::to_owned),
            frame,
            ..Node::default()
        }
    }

    /// A made-up element tree: each element's node and children, by number.
    struct Tree(HashMap<u32, (Node, Vec<u32>)>);

    impl Tree {
        fn new(items: Vec<(u32, Node, Vec<u32>)>) -> Tree {
            Tree(
                items
                    .into_iter()
                    .map(|(id, node, kids)| (id, (node, kids)))
                    .collect(),
            )
        }

        /// The elements listed from `root` within `WINDOW` while `in_time`
        /// says so, by number, and how many elements were read.
        fn walk_while(
            &self,
            root: u32,
            in_time: impl FnMut() -> bool,
        ) -> (Vec<(u32, Element)>, usize) {
            let mut reads = 0;
            let read = |id: &u32| {
                reads += 1;
                self.0.get(id).map(|(node, _)| node.clone())
            };
            let children = |id: &u32, _: &Node| {
                let kids = self.0.get(id).map(|(_, kids)| kids.clone());
                kids.unwrap_or_default()
            };
            let found = walk(root, WINDOW, read, children, in_time);
            (found, reads)
        }

        fn walk(&self, root: u32) -> Vec<(u32, Element)> {
            self.walk_while(root, || true).0
        }
    }

    const WINDOW: Bounds = Bounds {
        x: 0.0,
        y: 0.0,
        width: 500.0,
        height: 500.0,
    };

    /// One element of a made-up tree: its number, role, title, value, place
    /// in whole points, and its children's numbers.
    fn item(
        id: u32,
        role: &str,
        title: &str,
        value: Option<&str>,
        [x, y, width, height]: [u16; 4],
        kids: &[u32],
    ) -> (u32, Node, Vec<u32>) {
        let frame = rect(x.into(), y.into(), width.into(), height.into());
        (id, node(role, title, value, Some(frame)), kids.to_vec())
    }

    #[test]
    fn a_listing_reads_in_order_and_leaves_out_empty_containers() {
        let tree = Tree::new(vec![
            item(1, "AXWindow", "Notes", None, [0, 0, 500, 500], &[2, 5]),
            item(2, "AXGroup", "", None, [0, 0, 200, 50], &[3, 4]),
            item(3, "AXButton", "OK", None, [10, 10, 40, 20], &[]),
            item(4, "AXStaticText", "", Some("Hello"), [60, 10, 40, 20], &[]),
            item(5, "AXTextField", "", None, [0, 60, 200, 20], &[]),
        ]);
        let found = tree.walk(1);
        let ids: Vec<u32> = found.iter().map(|(id, _)| *id).collect();
        // The unnamed group is walked but not listed.
        assert_eq!(ids, [1, 3, 4, 5]);
        assert_eq!(found[1].1.role, "button");
        assert_eq!(found[1].1.name, "OK");
        assert_eq!(found[2].1.role, "text");
        assert_eq!(found[2].1.value.as_deref(), Some("Hello"));
        // An empty field is listed, to type into.
        assert_eq!(found[3].1.role, "text field");
        assert!(found.iter().all(|(_, element)| element.key.is_empty()));
    }

    #[test]
    fn what_is_scrolled_out_of_view_is_skipped_with_all_it_holds() {
        let tree = Tree::new(vec![
            item(1, "AXWindow", "Mail", None, [0, 0, 500, 500], &[2, 9]),
            item(2, "AXScrollArea", "", None, [0, 100, 500, 200], &[3]),
            item(3, "AXList", "", None, [0, 0, 500, 5000], &[4, 5, 6]),
            item(4, "AXStaticText", "", Some("above"), [0, 40, 500, 20], &[]),
            item(5, "AXStaticText", "", Some("shown"), [0, 150, 500, 20], &[]),
            item(6, "AXGroup", "row", None, [0, 400, 500, 20], &[7]),
            item(7, "AXButton", "Reply", None, [0, 400, 50, 20], &[]),
            item(9, "AXButton", "Off window", None, [600, 10, 40, 20], &[]),
        ]);
        let ids: Vec<u32> = tree.walk(1).into_iter().map(|(id, _)| id).collect();
        // Only the window and the row inside the scroll area show.
        assert_eq!(ids, [1, 5]);
    }

    #[test]
    fn a_container_with_no_size_still_has_its_children_walked() {
        let mut tree = Tree::new(vec![
            item(1, "AXWindow", "", None, [0, 0, 500, 500], &[2, 4]),
            item(2, "AXGroup", "", None, [900, 900, 0, 0], &[3]),
            item(3, "AXButton", "Save", None, [10, 10, 40, 20], &[]),
        ]);
        let placeless = node("AXButton", "Placeless", None, None);
        tree.0.insert(4, (placeless, Vec::new()));
        let ids: Vec<u32> = tree.walk(1).into_iter().map(|(id, _)| id).collect();
        assert_eq!(ids, [3]);
    }

    #[test]
    fn a_password_fields_value_is_never_listed() {
        let mut secret = node(
            "AXTextField",
            "Password",
            Some("hunter2"),
            Some(rect(0.0, 0.0, 99.0, 20.0)),
        );
        secret.subrole = "AXSecureTextField".into();
        let element = element_of(&secret, rect(0.0, 0.0, 99.0, 20.0)).expect("a field is listed");
        assert_eq!(element.role, "password field");
        assert_eq!(element.name, "Password");
        assert_eq!(element.value, None);
    }

    #[test]
    fn a_long_value_is_cut_and_a_description_names_what_has_no_title() {
        let mut image = node("AXImage", "", Some(&"x".repeat(5_000)), None);
        image.description = "Profile photo".into();
        let element = element_of(&image, WINDOW).expect("a named image is listed");
        assert_eq!(element.name, "Profile photo");
        assert_eq!(
            element.value.map(|value| value.chars().count()),
            Some(TEXT_KEPT)
        );
    }

    #[test]
    fn a_listing_stops_at_its_limits() {
        let button = node("AXButton", "", None, Some(rect(10.0, 10.0, 10.0, 10.0)));
        let mut items = vec![(
            0,
            node("AXWindow", "", None, Some(WINDOW)),
            (1..=5_000).collect(),
        )];
        items.extend((1..=5_000).map(|id| (id, button.clone(), vec![])));
        let wide = Tree::new(items);
        assert_eq!(wide.walk(0).len(), LISTED_MOST);

        let group = node("AXGroup", "", None, Some(WINDOW));
        let deep = Tree::new(
            (0..200)
                .map(|id| (id, group.clone(), vec![id + 1]))
                .collect(),
        );
        // Each level is read once, the deepest without its children.
        assert_eq!(deep.walk_while(0, || true).1, DEPTH_MOST + 1);

        let mut checks = 0;
        let (found, _) = wide.walk_while(0, || {
            checks += 1;
            checks <= 10
        });
        // Out of time, the walk stops with what it has: the window and nine.
        assert_eq!(found.len(), 9);
    }

    #[test]
    fn roles_read_as_plain_words() {
        assert_eq!(role_name("AXPopUpButton", ""), "pop up button");
        assert_eq!(role_name("AXTextField", "AXSearchField"), "search field");
        assert_eq!(role_name("AXButton", "AXCloseButton"), "close button");
        assert_eq!(role_name("AXButton", "AXSortButton"), "button");
        assert_eq!(
            role_name("AXTextField", "AXSecureTextField"),
            "password field"
        );
        assert_eq!(role_name("AXStaticText", ""), "text");
        assert_eq!(role_name("AXURLField", ""), "url field");
        assert_eq!(role_name("", ""), "element");
    }

    #[test]
    fn an_element_met_again_keeps_its_key_and_no_key_is_given_twice() {
        let mut known = Known::default();
        assert_eq!(known.key_for(10, 1), "ax1");
        assert_eq!(
            known.key_for(11, 1),
            "ax2",
            "a shared hash is not a shared element"
        );
        assert_eq!(known.key_for(10, 1), "ax1");
        assert_eq!(known.get("ax2"), Some(&11));
        assert_eq!(known.get("e2"), None);
        assert_eq!(known.get("ax99"), None);

        for element in 100..(100 + KNOWN_KEPT as u32) {
            known.key_for(element, u64::from(element));
        }
        assert_eq!(known.get("ax1"), None, "the oldest are dropped");
        let next = known.key_for(10, 1);
        assert_ne!(next, "ax1", "a dropped element comes back under a new key");
        assert!(known.by_key.len() <= KNOWN_KEPT);
        assert!(
            known
                .by_hash
                .values()
                .flatten()
                .all(|key| known.by_key.contains_key(key))
        );
    }

    fn window(pid: u32, owner: &str, layer: i64, bounds: Bounds, alpha: f64) -> Window {
        Window {
            id: pid * 10,
            pid,
            owner: owner.into(),
            layer,
            bounds,
            alpha,
        }
    }

    /// A screen, front first: the menu bar, a see-through overlay over it
    /// all, the editor's two processes' windows, the mail app's, and a
    /// window too small to see.
    fn screen() -> Vec<Window> {
        vec![
            window(50, "Window Server", 25, rect(0.0, 0.0, 1500.0, 24.0), 1.0),
            window(99, "Dimmer", 0, rect(0.0, 0.0, 1500.0, 1000.0), 0.0),
            window(10, "Editor", 0, rect(0.0, 24.0, 700.0, 700.0), 1.0),
            window(20, "Mail", 0, rect(600.0, 24.0, 700.0, 700.0), 1.0),
            window(11, "Editor", 0, rect(100.0, 800.0, 300.0, 100.0), 1.0),
            window(40, "Sliver", 0, rect(5.0, 5.0, 1.0, 1.0), 1.0),
        ]
    }

    fn identify(pid: u32, owner: &str) -> String {
        match pid {
            10 | 11 => "com.example.Editor".into(),
            20 => "com.example.mail".into(),
            _ => owner.to_lowercase(),
        }
    }

    #[test]
    fn apps_come_front_first_with_an_apps_processes_together() {
        let apps = apps_of(&screen(), identify);
        let listed: Vec<(&str, &str, &[u32])> = apps
            .iter()
            .map(|app| (app.id.as_str(), app.name.as_str(), app.pids.as_slice()))
            .collect();
        assert_eq!(
            listed,
            [
                ("com.example.Editor", "Editor", &[10, 11][..]),
                ("com.example.mail", "Mail", &[20][..]),
            ],
            "the menu bar, a see-through window and a sliver are no apps to use"
        );
    }

    #[test]
    fn the_window_under_a_spot_is_the_one_on_top_there() {
        let windows = screen();
        let apps = apps_of(&windows, identify);
        let (editor, mail) = (&apps[0], &apps[1]);
        let overlap = Point { x: 650.0, y: 100.0 };
        assert_eq!(window_at(&windows, overlap).map(|w| w.pid), Some(10));
        assert_eq!(over(&windows, overlap, editor), Ok(()));
        let refused = over(&windows, overlap, mail).expect_err("the editor is on top there");
        assert!(
            refused.contains("Editor's window is on top at 650,100, not Mail's"),
            "{refused}"
        );
        let bar = Point { x: 10.0, y: 10.0 };
        assert_eq!(window_at(&windows, bar).map(|w| w.pid), Some(50));
        assert!(
            over(&windows, bar, editor).is_err(),
            "a click on the menu bar is not the app's"
        );
        let desktop = Point {
            x: 1400.0,
            y: 950.0,
        };
        assert_eq!(window_at(&windows, desktop), None);
        assert!(over(&windows, desktop, editor).is_err_and(|why| why.contains("no window")));
        let edge = Point { x: 700.0, y: 500.0 };
        assert_eq!(
            window_at(&windows, edge).map(|w| w.pid),
            Some(20),
            "a window ends before its far edge"
        );
    }

    #[test]
    fn nothing_is_sent_unless_the_app_is_in_front() {
        let editor = App {
            id: "com.example.Editor".into(),
            name: "Editor".into(),
            pids: vec![10, 11],
        };
        assert_eq!(in_front(&editor, Some(11)), Ok(11));
        assert!(
            in_front(&editor, Some(20)).is_err_and(|why| why.contains("Editor is not in front"))
        );
        assert!(in_front(&editor, None).is_err());
    }

    #[test]
    fn rectangles_meet_and_hold_each_other() {
        let a = rect(0.0, 0.0, 100.0, 100.0);
        let b = rect(50.0, 80.0, 100.0, 100.0);
        assert!(overlaps(a, b));
        assert!(
            !overlaps(a, rect(100.0, 0.0, 10.0, 10.0)),
            "touching edges do not overlap"
        );
        assert_eq!(intersection(a, b), rect(50.0, 80.0, 50.0, 20.0));
        assert_eq!(union([a, b]), Some(rect(0.0, 0.0, 150.0, 180.0)));
        assert_eq!(
            union([rect(-1440.0, 0.0, 1440.0, 900.0), a]),
            Some(rect(-1440.0, 0.0, 1540.0, 900.0))
        );
        assert_eq!(union([]), None);
        assert_eq!(middle(b), Point { x: 100.0, y: 130.0 });
    }

    fn proc(pid: u32, ppid: u32, name: &str) -> Proc {
        Proc {
            pid,
            ppid,
            name: name.into(),
        }
    }

    /// Letronna (202) in tmux, whose server (200) was left by the terminal
    /// that started it; tmux clients in Terminal (103) and iTerm2 (302); an
    /// app that has nothing to do with it (400).
    fn table() -> Vec<Proc> {
        vec![
            proc(1, 0, "launchd"),
            proc(100, 1, "Terminal"),
            proc(101, 100, "login"),
            proc(102, 101, "zsh"),
            proc(103, 102, "tmux"),
            proc(200, 1, "tmux"),
            proc(201, 200, "zsh"),
            proc(202, 201, "letronna"),
            proc(300, 1, "iTerm2"),
            proc(301, 300, "zsh"),
            proc(302, 301, "tmux"),
            proc(400, 1, "Mail"),
            proc(401, 400, "helper"),
            proc(502, 102, "letronna"),
        ]
    }

    #[test]
    fn a_letronna_in_tmux_is_shown_by_every_terminal_with_a_tmux_client() {
        let mut hosts = hosts_of(202, &table());
        hosts.sort_unstable();
        assert_eq!(hosts, [100, 101, 102, 103, 200, 201, 300, 301, 302]);
    }

    #[test]
    fn a_letronna_in_a_terminal_is_shown_by_that_terminal_alone() {
        assert_eq!(hosts_of(502, &table()), [102, 101, 100]);
        assert_eq!(
            hosts_of(9_999, &table()),
            Vec::<u32>::new(),
            "an unknown process has no hosts"
        );
    }

    #[test]
    fn a_loop_in_the_process_table_ends_the_walk() {
        let table = [
            proc(600, 601, "a"),
            proc(601, 602, "b"),
            proc(602, 601, "c"),
        ];
        let by_pid: HashMap<u32, &Proc> = table.iter().map(|proc| (proc.pid, proc)).collect();
        assert_eq!(ancestors(600, &by_pid), [601, 602]);
    }

    #[test]
    fn a_program_belongs_to_its_innermost_app_bundle() {
        assert_eq!(
            bundle_of("/Applications/Safari.app/Contents/MacOS/Safari"),
            Some("/Applications/Safari.app")
        );
        let helper = "/Applications/Big Browser.app/Contents/Frameworks/Engine.framework/\
                      Helpers/Big Browser Helper.app/Contents/MacOS/Big Browser Helper";
        assert!(
            bundle_of(helper).is_some_and(|bundle| bundle.ends_with("/Big Browser Helper.app"))
        );
        assert_eq!(bundle_of("/usr/bin/login"), None);
        assert_eq!(
            bundle_of("/opt/tools.app/bin/tool"),
            None,
            "no Contents, no bundle"
        );
        assert_eq!(
            program("/Users/someone/target/debug/letronna-gui"),
            "letronna-gui"
        );
        assert_eq!(program("tmux"), "tmux");
    }

    #[test]
    fn the_permission_is_named_for_whoever_macos_counts_it_under() {
        let chain = |items: &[(&str, &str)]| -> Vec<(String, String)> {
            items
                .iter()
                .map(|(name, path)| (name.to_string(), path.to_string()))
                .collect()
        };
        let terminal = (
            "Terminal",
            "/System/Applications/Utilities/Terminal.app/Contents/MacOS/Terminal",
        );
        let app = "/Applications/Letronna.app/Contents/MacOS/letronna-gui";
        assert_eq!(grantee(Some(app), &chain(&[terminal])), "Letronna");
        let shell = ("zsh", "/bin/zsh");
        let tmux = ("tmux", "/opt/homebrew/bin/tmux");
        assert_eq!(
            grantee(Some("/usr/local/bin/letronna"), &chain(&[shell, tmux])),
            "tmux, or the terminal app tmux was started from"
        );
        assert_eq!(
            grantee(
                None,
                &chain(&[shell, ("login", "/usr/bin/login"), terminal])
            ),
            "Terminal"
        );
        assert_eq!(grantee(None, &[]), "the terminal app Letronna runs in");
    }

    #[test]
    fn a_missing_permission_says_which_and_where_to_turn_it_on() {
        let access = missing(Permission::Accessibility, "Terminal");
        assert!(access.contains("the Accessibility permission"), "{access}");
        assert!(
            access.contains("System Settings > Privacy & Security > Accessibility, for Terminal.")
        );
        let screen = missing(Permission::ScreenRecording, "Letronna");
        assert!(
            screen.contains("the Screen Recording permission, which screenshots need"),
            "{screen}"
        );
        assert!(screen.contains(
            "Privacy & Security > Screen & System Audio Recording, for Letronna, then quit and reopen it."
        ));
    }

    #[test]
    fn keys_are_found_where_a_us_keyboard_has_them() {
        assert_eq!(keycode("enter"), Some(0x24));
        assert_eq!(keycode("return"), Some(0x24));
        assert_eq!(keycode("backspace"), Some(0x33));
        assert_eq!(
            keycode("delete"),
            Some(0x75),
            "delete deletes forward, as elsewhere"
        );
        assert_eq!(keycode("f12"), Some(0x6F));
        assert_eq!(keycode("a"), Some(0x00));
        assert_eq!(keycode("s"), Some(0x01));
        assert_eq!(keycode("S"), Some(0x01));
        assert_eq!(keycode("`"), Some(0x32));
        assert_eq!(
            keycode("é"),
            None,
            "typed as text instead, and never with modifiers"
        );
        assert_eq!(keycode("ss"), None);
        assert_eq!(keycode(""), None);
        let flags = flags_of(&[Modifier::Command, Modifier::Shift]);
        assert_eq!(flags, 0x0010_0000 | 0x0002_0000);
        assert_eq!(flags_of(&[]), 0);
    }

    #[test]
    fn a_shortcut_is_matched_to_menus_as_they_record_it() {
        use Modifier::{Alt, Command, Control, Shift};
        assert_eq!(menu_modifiers(&[Command]), 0, "Command is assumed");
        assert_eq!(menu_modifiers(&[Shift, Command]), 1);
        assert_eq!(menu_modifiers(&[Command, Alt, Control]), 6);
        assert_eq!(menu_modifiers(&[Control]), 4 | 8);
    }

    #[test]
    fn the_macs_own_shortcuts_are_never_pressed() {
        use Modifier::{Alt, Command, Control, Shift};
        assert_eq!(system_shortcut("q", &[Shift, Command]), Some("logs out"));
        assert_eq!(
            system_shortcut("q", &[Command, Alt, Shift]),
            Some("logs out")
        );
        assert_eq!(
            system_shortcut("q", &[Control, Command]),
            Some("locks the screen")
        );
        assert_eq!(
            system_shortcut("escape", &[Alt, Command]),
            Some("opens Force Quit")
        );
        assert_eq!(
            system_shortcut("q", &[Command]),
            None,
            "quitting the app is the app's"
        );
        assert_eq!(system_shortcut("s", &[Command, Shift]), None);
        assert_eq!(system_shortcut("escape", &[]), None);
    }

    #[test]
    fn typed_text_goes_in_runs_with_returns_and_tabs_as_keys() {
        let units = |text: &str| Piece::Text(text.encode_utf16().collect());
        assert_eq!(
            pieces("hi\nthere\r\nok\tdone"),
            [
                units("hi"),
                Piece::Key(0x24),
                units("there"),
                Piece::Key(0x24),
                units("ok"),
                Piece::Key(0x30),
                units("done"),
            ]
        );
        let long = "a".repeat(45);
        assert_eq!(
            pieces(&long),
            [units(&long[..20]), units(&long[20..40]), units(&long[40..])]
        );
        let emoji = format!("{}😀", "a".repeat(19));
        assert_eq!(
            pieces(&emoji),
            [units(&"a".repeat(19)), units("😀")],
            "a character is never split between two events"
        );
        assert_eq!(pieces(""), []);
    }
}
