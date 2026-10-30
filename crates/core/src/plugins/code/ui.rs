//! What plugins put on screen: a panel drawn from a tree of a few plain
//! parts, a short status, and notices. A plugin describes them; the window
//! draws them with its own widgets and the terminal prints what it can, so
//! nothing a plugin sends is a page or a program, and the look stays
//! Letronna's.
//!
//! A plugin may send a new tree as often as it likes, so only the latest is
//! kept, and the front end is rung once however many arrive before it looks:
//! it redraws at most once a frame whatever a plugin does. A tree is held to
//! a size that draws in well under a frame.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, HashMap, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError};
use std::time::{Duration, Instant};

/// The most parts one tree has, and how deep it nests.
const MOST_NODES: usize = 500;
const MOST_DEPTH: usize = 16;
/// The bytes of one tree as sent, checked before it is read.
const TREE_BYTES: usize = 256 * 1024;
/// The characters one text keeps.
const TEXT_CHARS: usize = 2_000;
/// The characters a status keeps: a chip, or one part of the footer.
const STATUS_CHARS: usize = 60;
/// The characters a notice keeps, and how many wait to be shown.
const TOAST_CHARS: usize = 200;
const WAITING_TOASTS: usize = 20;
/// Notices one plugin may put up: a burst, then one a second.
const TOAST_BURST: f64 = 3.0;
const TOAST_EVERY: Duration = Duration::from_secs(1);

/// One part of a plugin's panel.
#[derive(Deserialize, Serialize, Clone, Debug, PartialEq)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum Node {
    /// Parts one under another.
    Column {
        #[serde(default)]
        children: Vec<Node>,
    },
    /// Parts side by side.
    Row {
        #[serde(default)]
        children: Vec<Node>,
    },
    Text {
        text: String,
        #[serde(default)]
        muted: bool,
        #[serde(default)]
        mono: bool,
        #[serde(default)]
        size: Size,
        #[serde(default)]
        tone: Tone,
    },
    /// Pressing it tells the plugin its `id`.
    Button {
        id: String,
        label: String,
        #[serde(default)]
        primary: bool,
        #[serde(default)]
        disabled: bool,
    },
    /// Flipping it tells the plugin its `id` and the new value.
    Switch {
        id: String,
        label: String,
        #[serde(default)]
        value: bool,
    },
    /// Pressing Enter in it tells the plugin its `id` and the text.
    Input {
        id: String,
        #[serde(default)]
        placeholder: String,
        #[serde(default)]
        value: String,
    },
    /// A bar, from 0 to 1.
    Progress {
        value: f64,
    },
    Divider,
    /// Opens `url`, an https address, in the browser.
    Link {
        label: String,
        url: String,
    },
}

#[derive(Deserialize, Serialize, Default, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Size {
    Small,
    #[default]
    Normal,
    Large,
}

/// The colour a text takes, from the theme's.
#[derive(Deserialize, Serialize, Default, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Tone {
    #[default]
    Default,
    Success,
    Warning,
    Error,
}

/// What a plugin shows now.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Surfaces {
    pub panel: Option<Arc<Node>>,
    /// Empty when it shows none.
    pub status: String,
}

/// A notice a plugin put up.
#[derive(Clone, Debug, PartialEq)]
pub struct Toast {
    pub plugin: String,
    pub text: String,
    pub kind: Kind,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Info,
    Warning,
    Error,
}

/// What each plugin shows, by name.
static SHOWN: Mutex<BTreeMap<String, Surfaces>> = Mutex::new(BTreeMap::new());
/// Counts every change to `SHOWN`, so a front end that looks now and then
/// can tell whether anything changed with one load.
static GENERATION: AtomicU64 = AtomicU64::new(0);
/// Notices not shown yet, oldest first.
static TOASTS: Mutex<VecDeque<Toast>> = Mutex::new(VecDeque::new());

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The bell a front end waits on: one ring stands for any number of
/// changes, so it never queues up behind a busy plugin.
fn bell() -> &'static (async_channel::Sender<()>, async_channel::Receiver<()>) {
    static BELL: OnceLock<(async_channel::Sender<()>, async_channel::Receiver<()>)> =
        OnceLock::new();
    BELL.get_or_init(|| async_channel::bounded(1))
}

fn ring() {
    let _ = bell().0.try_send(());
}

/// Rings whenever what a plugin shows changes or it puts up a notice.
pub fn listen() -> async_channel::Receiver<()> {
    bell().1.clone()
}

/// Changes to what plugins show so far.
pub fn generation() -> u64 {
    GENERATION.load(Ordering::Relaxed)
}

/// What every plugin shows now, by name.
pub fn snapshot() -> Vec<(String, Surfaces)> {
    lock(&SHOWN)
        .iter()
        .map(|(name, shown)| (name.clone(), shown.clone()))
        .collect()
}

/// The notices not shown yet, oldest first, which are now the caller's.
pub fn take_toasts() -> Vec<Toast> {
    lock(&TOASTS).drain(..).collect()
}

fn changed(change: impl FnOnce(&mut BTreeMap<String, Surfaces>)) {
    change(&mut lock(&SHOWN));
    GENERATION.fetch_add(1, Ordering::Relaxed);
    ring();
}

/// Shows `tree` as `plugin`'s panel, or why it cannot be.
pub(crate) fn set_panel(plugin: &str, tree: &Value) -> Result<(), String> {
    let tree = checked(tree)?;
    changed(|shown| {
        shown.entry(plugin.to_owned()).or_default().panel = Some(Arc::new(tree));
    });
    Ok(())
}

/// Shows `text` as `plugin`'s status, on one line and cut short; empty
/// shows none.
pub(crate) fn set_status(plugin: &str, text: &str) {
    let line = crate::tools::one_line(text);
    let status = cut(line.trim(), STATUS_CHARS);
    changed(|shown| {
        shown.entry(plugin.to_owned()).or_default().status = status;
    });
}

/// Takes away all `plugin` shows, as when it is switched off.
pub(crate) fn forget(plugin: &str) {
    if lock(&SHOWN).contains_key(plugin) {
        changed(|shown| {
            shown.remove(plugin);
        });
    }
}

/// Puts up `text` from `plugin`, unless it has put up more than its share
/// lately: false then, and nothing shows.
pub(crate) fn toast(plugin: &str, text: &str, kind: Kind) -> bool {
    if !allowance(plugin) {
        return false;
    }
    let text = cut(crate::tools::one_line(text).trim(), TOAST_CHARS);
    if text.is_empty() {
        return true;
    }
    let mut waiting = lock(&TOASTS);
    waiting.push_back(Toast {
        plugin: plugin.to_owned(),
        text,
        kind,
    });
    let over = waiting.len().saturating_sub(WAITING_TOASTS);
    waiting.drain(..over);
    drop(waiting);
    ring();
    true
}

/// Whether `plugin` may put up a notice now: a bucket of `TOAST_BURST`,
/// filling at one every `TOAST_EVERY`.
fn allowance(plugin: &str) -> bool {
    static BUCKETS: Mutex<Option<HashMap<String, (Instant, f64)>>> = Mutex::new(None);
    let mut buckets = lock(&BUCKETS);
    let now = Instant::now();
    let (since, left) = buckets
        .get_or_insert_with(HashMap::new)
        .entry(plugin.to_owned())
        .or_insert((now, TOAST_BURST));
    let earned = now.duration_since(*since).as_secs_f64() / TOAST_EVERY.as_secs_f64();
    *left = (*left + earned).min(TOAST_BURST);
    *since = now;
    if *left < 1.0 {
        return false;
    }
    *left -= 1.0;
    true
}

/// `text` cut to `most` characters, with an ellipsis when cut.
fn cut(text: &str, most: usize) -> String {
    match text.chars().count() > most {
        true => format!(
            "{}…",
            text.chars().take(most - 1).collect::<String>().trim_end()
        ),
        false => text.to_owned(),
    }
}

/// `tree` read as a panel, or why it cannot be shown.
fn checked(tree: &Value) -> Result<Node, String> {
    if tree.to_string().len() > TREE_BYTES {
        return Err(format!(
            "The panel is over {} KB; send less, or show part of it.",
            TREE_BYTES / 1024
        ));
    }
    let mut node: Node = serde_json::from_value(tree.clone())
        .map_err(|e| format!("The panel does not read as one: {e}"))?;
    let mut count = 0;
    let mut ids = Vec::new();
    walk(&mut node, 1, &mut count, &mut ids)?;
    Ok(node)
}

/// Checks `node` and what it holds, `depth` down, counting the parts and
/// gathering the ids, and cuts what is too long.
fn walk(
    node: &mut Node,
    depth: usize,
    count: &mut usize,
    ids: &mut Vec<String>,
) -> Result<(), String> {
    *count += 1;
    if *count > MOST_NODES {
        return Err(format!("The panel has more than {MOST_NODES} parts."));
    }
    if depth > MOST_DEPTH {
        return Err(format!("The panel nests deeper than {MOST_DEPTH}."));
    }
    let mut id = |id: &str| {
        if id.trim().is_empty() {
            return Err("A part the user can press or type in has no id.".to_owned());
        }
        if ids.iter().any(|known| known == id) {
            return Err(format!("Two parts of the panel have the id {id}."));
        }
        ids.push(id.to_owned());
        Ok(())
    };
    match node {
        Node::Column { children } | Node::Row { children } => {
            for child in children {
                walk(child, depth + 1, count, ids)?;
            }
        }
        Node::Text { text, .. } => *text = cut(text, TEXT_CHARS),
        Node::Button {
            id: name, label, ..
        }
        | Node::Switch {
            id: name, label, ..
        } => {
            id(name)?;
            *label = cut(&crate::tools::one_line(label), TEXT_CHARS);
        }
        Node::Input {
            id: name, value, ..
        } => {
            id(name)?;
            *value = cut(value, TEXT_CHARS);
        }
        Node::Progress { value } => {
            *value = if value.is_finite() {
                value.clamp(0.0, 1.0)
            } else {
                0.0
            }
        }
        Node::Divider => {}
        Node::Link { label, url } => {
            if !url.starts_with("https://") {
                return Err(format!("The link {url} is not an https address."));
            }
            *label = cut(&crate::tools::one_line(label), TEXT_CHARS);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_tree_reads_and_too_big_or_unknown_ones_are_refused_with_a_reason() {
        let tree = json!({"type": "column", "children": [
            {"type": "text", "text": "25:00", "size": "large"},
            {"type": "row", "children": [
                {"type": "button", "id": "start", "label": "Start", "primary": true},
                {"type": "switch", "id": "sound", "label": "Sound", "value": true},
            ]},
            {"type": "input", "id": "task", "placeholder": "What now"},
            {"type": "progress", "value": 3.0},
            {"type": "divider"},
            {"type": "link", "label": "Docs", "url": "https://example.com"},
        ]});
        let Node::Column { children } = checked(&tree).unwrap() else {
            panic!("not a column");
        };
        assert_eq!(children[3], Node::Progress { value: 1.0 });

        let many: Vec<Value> = (0..MOST_NODES)
            .map(|_| json!({"type": "divider"}))
            .collect();
        let error = checked(&json!({"type": "column", "children": many})).unwrap_err();
        assert!(error.contains("more than 500 parts"), "{error}");
        let error = checked(&json!({"type": "marquee"})).unwrap_err();
        assert!(error.contains("does not read"), "{error}");
        let twice = json!({"type": "row", "children": [
            {"type": "button", "id": "a", "label": "A"},
            {"type": "button", "id": "a", "label": "B"},
        ]});
        assert!(checked(&twice).unwrap_err().contains("id a"));
        let plain = json!({"type": "link", "label": "x", "url": "http://example.com"});
        assert!(checked(&plain).unwrap_err().contains("https"));
        let mut deep = json!({"type": "divider"});
        for _ in 0..MOST_DEPTH {
            deep = json!({"type": "column", "children": [deep]});
        }
        assert!(checked(&deep).unwrap_err().contains("deeper"));
    }

    #[test]
    fn a_status_is_one_line_and_cut_and_changes_ring_once() {
        let listening = listen();
        while listening.try_recv().is_ok() {}
        let before = generation();
        set_status("status-test", "first line\nsecond line");
        set_status("status-test", &"x".repeat(100));
        assert_eq!(generation(), before + 2);
        // Two changes, one ring: the front end looks once and sees both.
        assert!(listening.try_recv().is_ok());
        assert!(listening.try_recv().is_err());
        let shown = snapshot()
            .into_iter()
            .find(|(name, _)| name == "status-test")
            .unwrap()
            .1;
        assert_eq!(shown.status.chars().count(), STATUS_CHARS);
        assert!(shown.status.ends_with('…'));
        set_status("status-test", "first line\nsecond line");
        let shown = snapshot()
            .into_iter()
            .find(|(name, _)| name == "status-test")
            .unwrap()
            .1;
        assert_eq!(shown.status, "first line second line");
        forget("status-test");
        assert!(snapshot().iter().all(|(name, _)| name != "status-test"));
    }

    #[test]
    fn notices_are_held_to_a_burst_then_one_a_second() {
        let plugin = "toast-test";
        let put: Vec<bool> = (0..5)
            .map(|n| toast(plugin, &format!("{n}"), Kind::Info))
            .collect();
        assert_eq!(put, [true, true, true, false, false]);
        let mine: Vec<Toast> = take_toasts()
            .into_iter()
            .filter(|toast| toast.plugin == plugin)
            .collect();
        assert_eq!(mine.len(), 3);
    }
}
