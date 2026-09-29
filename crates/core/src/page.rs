//! A page as the model reads it: the browser's accessibility tree, cut down
//! to what a person could use or read, one element a line, each with a ref
//! (`e12`) the model names to click or type.
//!
//! That is a few kilobytes where the HTML is hundreds, and it is what the
//! browser itself takes each thing to be, so a styled `<div>` button still
//! reads as a button. Every line gets a ref, so any text can be clicked, and
//! a ref is never reused while the browser is open: one read off an earlier
//! page fails rather than naming something else. Words read as sentences,
//! with the links in them inline, and a page too long to show at once is
//! shown a window at a time.

use serde::Deserialize;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;

/// How much of a page one listing shows. Under the chat's limit for a tool
/// result, so a listing is never cut in the middle on its way to the model.
pub(crate) const PAGE_CHARS: usize = 6_000;
/// Lines shown after each line `find` matches.
const AFTER_MATCH: usize = 2;
/// Names longer than this are cut: a link's name can be a whole paragraph.
const NAME_CHARS: usize = 100;
/// A line of page text longer than this is cut.
const TEXT_CHARS: usize = 1_000;
/// Lines one frame adds to a listing at most: a page embedding a long
/// document still lists its own content in full.
const FRAME_LINES: usize = 200;
/// How deep the listing indents before it stops adding more.
const MAX_INDENT: usize = 10;
/// How deep the tree is walked. Real pages stay well under this; a page
/// built to be deeper would otherwise run the walk out of stack.
const MAX_DEPTH: usize = 400;
/// Roles skipped with everything inside them: pieces of text layout, and
/// list bullets.
const SKIPPED: &[&str] = &["InlineTextBox", "LineBreak", "ListMarker"];
/// Roles of an element a frame is shown in. None gets a ref: what is done
/// to one lands inside its frame, whose own refs under its header say
/// which frame an action is for.
const FRAME_OWNERS: &[&str] = &[
    "Iframe",
    "IframePresentational",
    "EmbeddedObject",
    "PluginObject",
];
/// Parts of a page worth a line for the shape they give it.
const LANDMARKS: &[&str] = &[
    "banner",
    "navigation",
    "main",
    "contentinfo",
    "complementary",
    "search",
    "form",
    "region",
    "dialog",
    "alertdialog",
    "alert",
    "status",
    "log",
];
/// Roles that sit inside a line of words rather than starting one: bold,
/// code, a footnote mark, a span.
const INLINE: &[&str] = &[
    "generic",
    "none",
    "emphasis",
    "strong",
    "code",
    "mark",
    "superscript",
    "subscript",
    "time",
    "abbr",
    "insertion",
    "deletion",
];
/// Where a page speaks up. Their words are shown even in the short listing:
/// "Wrong password" is what the model most needs to see.
const SPOKEN: &[&str] = &["dialog", "alertdialog", "alert", "status", "log"];
/// Things a person uses.
const CONTROLS: &[&str] = &[
    "link",
    "button",
    "textbox",
    "searchbox",
    "combobox",
    "listbox",
    "option",
    "checkbox",
    "radio",
    "switch",
    "slider",
    "spinbutton",
    "menu",
    "menubar",
    "menuitem",
    "menuitemcheckbox",
    "menuitemradio",
    "tab",
    "treeitem",
    "DisclosureTriangle",
    "ColorWell",
    "Date",
    "DateTime",
    "InputTime",
];
/// Elements whose insides are part of the element: a text field's inner
/// box, a dropdown's options, a picture.
const SEALED: &[&str] = &[
    "textbox",
    "searchbox",
    "combobox",
    "checkbox",
    "radio",
    "switch",
    "slider",
    "spinbutton",
    "option",
    "progressbar",
    "meter",
    "image",
    "img",
];
/// Elements whose insides are usually just their label -- unless something
/// in there can be used by itself, such as the link inside a heading.
const LABELLED: &[&str] = &[
    "link",
    "button",
    "heading",
    "tab",
    "menuitem",
    "menuitemcheckbox",
    "menuitemradio",
    "DisclosureTriangle",
];
/// States worth saying, in the order they are said. Not focus: the model
/// knows what it just clicked, and a listing that changed only by that
/// would not read as "nothing changed".
const STATES: &[&str] = &[
    "level", "checked", "pressed", "selected", "expanded", "disabled", "required", "invalid",
];
/// Refs handed out for the page open now: which node each names, and how
/// its line reads, for the approval card. Numbers keep counting across
/// pages, so a ref from an earlier page never names something on this one.
#[derive(Default)]
pub(crate) struct Refs {
    last: u32,
    /// Keyed by the frame's document as well: nodes in two processes can
    /// share a number.
    by_node: HashMap<(String, i64), u32>,
    nodes: HashMap<u32, (Option<Arc<Frame>>, i64)>,
    /// How each element's line reads, and how much of that names it: the
    /// rest is the value it holds.
    labels: HashMap<u32, (String, usize)>,
}

impl Refs {
    fn of(&mut self, frame: Option<&Arc<Frame>>, node: i64) -> u32 {
        let key = (frame.map(|frame| frame.key()).unwrap_or_default(), node);
        if let Some(number) = self.by_node.get(&key) {
            return *number;
        }
        self.last += 1;
        self.by_node.insert(key, self.last);
        self.nodes.insert(self.last, (frame.cloned(), node));
        self.last
    }

    /// The node a ref names, and the frame it is in when not the page.
    pub(crate) fn place(&self, number: u32) -> Option<(Option<&Arc<Frame>>, i64)> {
        let (frame, node) = self.nodes.get(&number)?;
        Some((frame.as_ref(), *node))
    }

    /// How the line of the element a ref names reads.
    pub(crate) fn label(&self, number: u32) -> Option<&str> {
        self.labels.get(&number).map(|(label, _)| label.as_str())
    }

    /// What an element is, found by its node: its line without the value it
    /// holds, which an action on it is about to change.
    pub(crate) fn name_for(&self, frame: Option<&Frame>, node: i64) -> Option<&str> {
        let key = (frame.map(Frame::key).unwrap_or_default(), node);
        let (label, named) = self.labels.get(self.by_node.get(&key)?)?;
        label.get(..*named)
    }

    /// A new page: its nodes are new, even where their numbers are not.
    pub(crate) fn forget(&mut self) {
        self.by_node.clear();
        self.nodes.clear();
        self.labels.clear();
    }
}

/// A frame whose elements a listing showed, as it was then: they are
/// reached through its session, and only while it shows the same document.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Frame {
    /// The browser's id for the frame.
    pub(crate) id: String,
    /// Its document when listed: another means it has loaded a new page.
    pub(crate) loader: String,
    pub(crate) origin: String,
    /// Whether its origin is not the page's.
    pub(crate) foreign: bool,
    /// The session its elements are reached through: its own, or that of
    /// the frame whose process it shares; `None` for the page's.
    pub(crate) session: Option<String>,
    /// Each frame from the page down to this one: the session holding the
    /// element it is shown in, its id, and the session of its own elements.
    pub(crate) path: Vec<(Option<String>, String, Option<String>)>,
}

impl Frame {
    fn key(&self) -> String {
        let session = self.session.as_deref().unwrap_or_default();
        format!("{} {} {session}", self.id, self.loader)
    }

    /// Its header in a listing. Not a line the page could write: those all
    /// start with a dash.
    pub(crate) fn header(&self) -> String {
        let whose = if self.foreign {
            "cross-origin"
        } else {
            "same origin"
        };
        format!("frame {} ({whose})", self.origin)
    }
}

/// The number in a ref as the model writes it: `e12`, `[e12]` or `12`.
pub(crate) fn ref_number(raw: &str) -> Option<u32> {
    let bare = raw.trim().trim_start_matches('[').trim_end_matches(']');
    bare.trim_start_matches(['e', 'E']).parse().ok()
}

/// The page's accessibility tree, as `Accessibility.getFullAXTree` gives it,
/// read into just the fields used.
#[derive(Deserialize)]
pub(crate) struct Tree {
    nodes: Vec<Node>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Node {
    node_id: String,
    #[serde(default)]
    ignored: bool,
    role: Option<Named>,
    name: Option<Named>,
    value: Option<Named>,
    #[serde(default)]
    properties: Vec<Property>,
    #[serde(default)]
    child_ids: Vec<String>,
    #[serde(rename = "backendDOMNodeId")]
    backend: Option<i64>,
}

#[derive(Deserialize)]
struct Named {
    #[serde(default)]
    value: Value,
}

#[derive(Deserialize)]
struct Property {
    name: String,
    value: Named,
}

impl Node {
    fn role(&self) -> &str {
        self.role
            .as_ref()
            .and_then(|r| r.value.as_str())
            .unwrap_or_default()
    }

    /// The name as written on the page, spaces and all.
    fn raw_name(&self) -> &str {
        self.name
            .as_ref()
            .and_then(|n| n.value.as_str())
            .unwrap_or_default()
    }

    fn name(&self) -> String {
        words(self.raw_name())
    }

    fn property(&self, name: &str) -> Option<&Value> {
        self.properties
            .iter()
            .find(|p| p.name == name)
            .map(|p| &p.value.value)
    }

    fn is(&self, name: &str) -> bool {
        self.property(name)
            .is_some_and(|v| v == true || v.as_str().is_some_and(|s| !s.is_empty() && s != "false"))
    }
}

/// Text with every run of spaces and line breaks made one space.
fn words(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// One line of a listing, and every node it stands for, so a point on the
/// page can be found in the listing.
pub(crate) struct Line {
    text: String,
    /// For words with links among them, the words alone, so a phrase that
    /// runs across a link can still be found.
    plain: Option<String>,
    nodes: Vec<i64>,
}

/// Words being gathered into one line.
#[derive(Default)]
struct Run {
    shown: String,
    plain: String,
    /// The first node holding a word, which the line's own ref names.
    first: Option<i64>,
    nodes: Vec<i64>,
    links: bool,
}

/// A page as read: its lines, what it is called, and where each node's
/// parent is, to find a line from a point on the page.
pub(crate) struct Page {
    pub(crate) title: String,
    pub(crate) url: String,
    pub(crate) lines: Vec<Line>,
    parents: HashMap<i64, i64>,
    /// The line where the page's main content starts, when it marks one.
    pub(crate) main: Option<usize>,
}

impl Page {
    pub(crate) fn text(&self) -> String {
        self.lines
            .iter()
            .map(|l| l.text.as_str())
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// "Title — address", as cards and headers name the page.
    pub(crate) fn named(&self) -> String {
        match self.title.is_empty() {
            true => self.url.clone(),
            false => format!("{} — {}", self.title, self.url),
        }
    }

    /// Whether the whole listing fits in one go.
    pub(crate) fn fits(&self) -> bool {
        self.lines.iter().map(|l| l.text.len() + 1).sum::<usize>() <= PAGE_CHARS
    }

    /// The line holding `node`, or the nearest holding one of its ancestors.
    pub(crate) fn line_of(&self, node: i64) -> Option<usize> {
        let mut node = node;
        for _ in 0..MAX_DEPTH {
            if let Some(at) = self
                .lines
                .iter()
                .position(|line| line.nodes.contains(&node))
            {
                return Some(at);
            }
            node = *self.parents.get(&node)?;
        }
        None
    }
}

/// The tree, read into lines.
/// `frame` is the frame the tree is from, if not the page itself, and
/// `inner` the listings of the frames in it, by the node of the element
/// each is shown in, to go where that element is.
pub(crate) fn page(
    tree: &Tree,
    full: bool,
    refs: &mut Refs,
    frame: Option<&Arc<Frame>>,
    inner: HashMap<i64, Vec<Line>>,
) -> Page {
    let tree = tree.nodes.as_slice();
    let index: HashMap<&str, usize> = tree
        .iter()
        .enumerate()
        .map(|(at, node)| (node.node_id.as_str(), at))
        .collect();
    let mut parents = HashMap::new();
    for node in tree {
        let Some(parent) = node.backend else {
            continue;
        };
        for child in &node.child_ids {
            if let Some(child) = index.get(child.as_str()).and_then(|at| tree[*at].backend) {
                parents.insert(child, parent);
            }
        }
    }
    let mut walk = Walk {
        tree,
        index,
        refs,
        frame,
        inner,
        lines: Vec::new(),
        main: None,
    };
    if !tree.is_empty() {
        walk.children(0, 0, full, 0);
    }
    let root = tree.first();
    Page {
        title: root.map(Node::name).unwrap_or_default(),
        url: root
            .and_then(|root| root.property("url"))
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned(),
        lines: walk.lines,
        parents,
        main: walk.main,
    }
}

struct Walk<'a> {
    tree: &'a [Node],
    index: HashMap<&'a str, usize>,
    refs: &'a mut Refs,
    frame: Option<&'a Arc<Frame>>,
    inner: HashMap<i64, Vec<Line>>,
    lines: Vec<Line>,
    main: Option<usize>,
}

impl Walk<'_> {
    fn child_nodes(&self, at: usize) -> impl Iterator<Item = usize> + '_ {
        self.tree[at]
            .child_ids
            .iter()
            .filter_map(|id| self.index.get(id.as_str()).copied())
    }

    /// The children of `at`, one after another. Where they are words --
    /// "Rust is a ", a link, " language" -- they read as one line with the
    /// links inline: `Rust is a [general-purpose](e45) language`. Broken at
    /// every link, a sentence reads as a list of scraps, and no phrase in it
    /// can be found. Links that are all there is, as in a menu, keep lines
    /// of their own.
    fn children(&mut self, at: usize, indent: usize, text: bool, depth: usize) {
        if depth > MAX_DEPTH {
            return;
        }
        let kids: Vec<usize> = self.child_nodes(at).collect();
        let prose = text && kids.iter().any(|kid| self.is_text(*kid));
        let mut run = Run::default();
        for kid in kids {
            if self.is_text(kid) || (prose && self.is_inline(kid, depth)) {
                if text {
                    self.absorb(kid, &mut run, depth);
                }
                continue;
            }
            self.flush(&mut run, indent);
            self.visit(kid, indent, text, depth + 1);
        }
        self.flush(&mut run, indent);
    }

    fn is_text(&self, at: usize) -> bool {
        let node = &self.tree[at];
        !node.ignored && node.role() == "StaticText"
    }

    /// Whether `at` belongs in a line of words: a link that is only words,
    /// or something holding nothing else -- bold, code, a footnote mark.
    fn is_inline(&self, at: usize, depth: usize) -> bool {
        let node = &self.tree[at];
        let role = node.role();
        if depth > MAX_DEPTH {
            return false;
        }
        if self.is_text(at) || SKIPPED.contains(&role) {
            return true;
        }
        if !node.ignored && role == "link" {
            return !self.usable_inside(at, depth);
        }
        let wrapper = node.ignored || (INLINE.contains(&role) && !shown(node));
        wrapper
            && self
                .child_nodes(at)
                .all(|kid| self.is_inline(kid, depth + 1))
    }

    /// Add `at` to a line of words: its text, or a link as `[name](e12)`.
    fn absorb(&mut self, at: usize, run: &mut Run, depth: usize) {
        let node = &self.tree[at];
        if SKIPPED.contains(&node.role()) || depth > MAX_DEPTH {
            return;
        }
        if self.is_text(at) {
            let piece = node.raw_name();
            run.shown.push_str(piece);
            run.plain.push_str(piece);
            if !piece.trim().is_empty() && run.first.is_none() {
                run.first = node.backend;
            }
            run.nodes.extend(node.backend);
            return;
        }
        if let Some(backend) = node
            .backend
            .filter(|_| !node.ignored && node.role() == "link")
        {
            let number = self.refs.of(self.frame, backend);
            self.refs.labels.insert(number, describe(node));
            let name = match node.name() {
                name if name.is_empty() => "link".to_owned(),
                name => cut(&name, NAME_CHARS),
            };
            run.shown.push_str(&format!("[{name}](e{number})"));
            run.plain.push_str(&name);
            run.links = true;
            run.nodes.push(backend);
            self.gather(at, depth, &mut run.nodes);
            return;
        }
        let kids: Vec<usize> = self.child_nodes(at).collect();
        for kid in kids {
            self.absorb(kid, run, depth + 1);
        }
    }

    /// End a line of words, if one was begun.
    fn flush(&mut self, run: &mut Run, indent: usize) {
        let run = std::mem::take(run);
        let said = cut(&words(&run.shown), TEXT_CHARS);
        let Some(first) = run.first.or(run.nodes.first().copied()) else {
            return;
        };
        if said.is_empty() {
            return;
        }
        let number = self.refs.of(self.frame, first);
        let plain = words(&run.plain);
        let label = format!("\"{}\"", cut(&plain, NAME_CHARS));
        self.refs
            .labels
            .insert(number, (label.clone(), label.len()));
        self.lines.push(Line {
            text: format!("{}- \"{said}\" [e{number}]", pad(indent)),
            plain: run.links.then_some(plain),
            nodes: run.nodes,
        });
    }

    fn visit(&mut self, at: usize, indent: usize, text: bool, depth: usize) {
        let node = &self.tree[at];
        let role = node.role();
        if SKIPPED.contains(&role) {
            return;
        }
        if let Some(lines) = node.backend.and_then(|backend| self.inner.remove(&backend)) {
            let pad = pad(indent);
            self.lines.extend(lines.into_iter().map(|line| Line {
                text: format!("{pad}{}", line.text),
                ..line
            }));
            return;
        }
        if FRAME_OWNERS.contains(&role) {
            // One with a name is said to be there, so a frame left out of
            // the listing is not taken for none.
            if !node.ignored && !node.raw_name().trim().is_empty() {
                let (label, _) = describe(node);
                self.lines.push(Line {
                    text: format!("{}- {label} (its frame is not listed)", pad(indent)),
                    plain: None,
                    nodes: node.backend.into_iter().collect(),
                });
            }
            return;
        }
        let Some(backend) = node.backend.filter(|_| !node.ignored && shown(node)) else {
            return self.children(at, indent, text, depth);
        };
        if role == "main" && self.main.is_none() {
            self.main = Some(self.lines.len());
        }
        let number = self.refs.of(self.frame, backend);
        let (label, named) = describe(node);
        self.refs.labels.insert(number, (label.clone(), named));
        self.lines.push(Line {
            text: format!("{}- {label} [e{number}]{}", pad(indent), states(node)),
            plain: None,
            nodes: vec![backend],
        });
        let whole =
            SEALED.contains(&role) || (LABELLED.contains(&role) && !self.usable_inside(at, depth));
        if whole {
            // Everything inside is this line, so a point in there finds it.
            let mut inside = Vec::new();
            self.gather(at, depth, &mut inside);
            if let Some(line) = self.lines.last_mut() {
                line.nodes.extend(inside);
            }
            return;
        }
        let text = text || SPOKEN.contains(&role);
        self.children(at, indent + 1, text, depth);
    }

    /// Whether something inside `at` can be used by itself.
    fn usable_inside(&self, at: usize, depth: usize) -> bool {
        depth <= MAX_DEPTH
            && self.child_nodes(at).any(|child| {
                let node = &self.tree[child];
                (!node.ignored && (CONTROLS.contains(&node.role()) || node.is("focusable")))
                    || self.usable_inside(child, depth + 1)
            })
    }

    /// Every node inside `at`.
    fn gather(&self, at: usize, depth: usize, into: &mut Vec<i64>) {
        if depth > MAX_DEPTH {
            return;
        }
        for child in self.child_nodes(at) {
            into.extend(self.tree[child].backend);
            self.gather(child, depth + 1, into);
        }
    }
}

/// Whether a node gets a line of its own.
fn shown(node: &Node) -> bool {
    let role = node.role();
    if LANDMARKS.contains(&role) || CONTROLS.contains(&role) || role == "heading" {
        return true;
    }
    let named = !node.raw_name().trim().is_empty();
    if matches!(role, "image" | "img") {
        return named;
    }
    // Chrome's marks for something that takes the keyboard: a `<div>` made
    // clickable, an editable area.
    role != "RootWebArea" && named && (node.is("focusable") || node.is("editable"))
}

/// A frame's listing as the page shows it: under its header, indented,
/// and cut at `FRAME_LINES`. One in another process keeps no nodes on its
/// lines: their numbers are that process's, and would be taken for the
/// page's.
pub(crate) fn framed(frame: &Frame, lines: Vec<Line>) -> Vec<Line> {
    let apart = frame.session.is_some();
    let more = lines.len().saturating_sub(FRAME_LINES);
    let bare = |text: String| Line {
        text,
        plain: None,
        nodes: Vec::new(),
    };
    let mut out = vec![bare(frame.header())];
    out.extend(lines.into_iter().take(FRAME_LINES).map(|line| Line {
        text: format!("{}{}", pad(1), line.text),
        plain: line.plain,
        nodes: if apart { Vec::new() } else { line.nodes },
    }));
    if more > 0 {
        out.push(bare(format!(
            "{}({more} more lines of this frame are not listed)",
            pad(1)
        )));
    }
    out
}

fn pad(indent: usize) -> String {
    "  ".repeat(indent.min(MAX_INDENT))
}

/// How an element reads -- its role, its name, and what is in it -- and how
/// much of that names it.
fn describe(node: &Node) -> (String, usize) {
    let role = match node.role() {
        "img" => "image",
        "DisclosureTriangle" => "disclosure",
        role => role,
    };
    let mut text = role.to_owned();
    let name = node.name();
    if !name.is_empty() {
        text.push_str(&format!(" \"{}\"", cut(&name, NAME_CHARS)));
    }
    let value = match node.value.as_ref().map(|v| &v.value) {
        Some(Value::String(value)) => words(value),
        Some(Value::Null) | None => String::new(),
        Some(other) => other.to_string(),
    };
    let named = text.len();
    if !value.is_empty() && value != name {
        text.push_str(&format!(" = \"{}\"", cut(&value, NAME_CHARS)));
    }
    (text, named)
}

/// The states worth saying about an element: checked, collapsed, disabled.
fn states(node: &Node) -> String {
    let mut out = String::new();
    for state in STATES {
        let Some(value) = node.property(state) else {
            continue;
        };
        let word = match (*state, value) {
            ("level", level) => format!("level {level}"),
            ("checked", Value::Bool(true)) => "checked".into(),
            ("checked", Value::String(said)) if said == "true" => "checked".into(),
            ("checked", Value::String(said)) if said == "mixed" => "partly checked".into(),
            ("checked", _) => "not checked".into(),
            ("expanded", Value::Bool(true)) => "expanded".into(),
            ("expanded", _) => "collapsed".into(),
            (state, _) if node.is(state) => state.into(),
            _ => continue,
        };
        out.push(' ');
        out.push_str(&word);
    }
    out
}

/// `text`, cut to `max` characters with an ellipsis.
pub(crate) fn cut(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_owned();
    }
    let mut cut: String = text.chars().take(max).collect();
    cut.push('…');
    cut
}

/// What the model is shown of `lines`: from `start` (counting from 0), as
/// many as fit, and a note on the rest.
pub(crate) fn window(lines: &[Line], start: usize) -> (String, Option<String>) {
    let start = start.min(lines.len().saturating_sub(1));
    let mut shown = String::new();
    let mut end = start;
    for line in &lines[start..] {
        if !shown.is_empty() && shown.len() + line.text.len() + 1 > PAGE_CHARS {
            break;
        }
        shown.push_str(&line.text);
        shown.push('\n');
        end += 1;
    }
    let total = lines.len();
    if start == 0 && end == total {
        return (shown, None);
    }
    let mut note = format!("Lines {}–{end} of {total} are shown", start + 1);
    if end < total {
        note.push_str(&format!("; look with from: {} for more", end + 1));
    }
    if start > 0 {
        note.push_str("; from: 1 starts at the top");
    }
    note.push('.');
    (shown, Some(note))
}

/// The lines containing `phrase`, each with the lines just after it -- a
/// label's value sits in the next cell, a heading's words under it -- as
/// many as fit, and how many matched.
pub(crate) fn matching(lines: &[Line], phrase: &str) -> (String, String) {
    let needle = phrase.to_lowercase();
    let found: Vec<usize> = (0..lines.len())
        .filter(|at| {
            let line = &lines[*at];
            let said = line.plain.as_deref().unwrap_or(&line.text);
            said.to_lowercase().contains(&needle)
        })
        .collect();
    let mut shown = String::new();
    let mut next = 0;
    'found: for at in &found {
        if *at > next && !shown.is_empty() {
            shown.push_str("…\n");
        }
        let end = (at + 1 + AFTER_MATCH).min(lines.len());
        for line in &lines[next.max(*at)..end] {
            let line = line.text.trim_start();
            if shown.len() + line.len() + 1 > PAGE_CHARS {
                break 'found;
            }
            shown.push_str(line);
            shown.push('\n');
        }
        next = end;
    }
    let note = match found.len() {
        0 => format!("No line on the page contains \"{phrase}\"."),
        n => format!("{n} of {} lines contain \"{phrase}\".", lines.len()),
    };
    (shown, note)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// A node of a test page: its id (also its DOM node), role, name and
    /// children.
    fn node(id: u32, role: &str, name: &str, kids: &[u32]) -> Value {
        json!({"nodeId": id.to_string(), "ignored": false, "role": {"value": role},
               "name": {"value": name}, "backendDOMNodeId": id,
               "childIds": kids.iter().map(|k| k.to_string()).collect::<Vec<_>>()})
    }

    fn with(mut node: Value, properties: &[(&str, Value)]) -> Value {
        node["properties"] = properties
            .iter()
            .map(|(name, value)| json!({"name": name, "value": {"value": value}}))
            .collect();
        node
    }

    fn ignored(mut node: Value) -> Value {
        node["ignored"] = true.into();
        node
    }

    fn valued(mut node: Value, value: &str) -> Value {
        node["value"] = json!({"value": value});
        node
    }

    /// Chrome's mark for an element that takes the keyboard.
    fn focusable() -> (&'static str, Value) {
        ("focusable", json!(true))
    }

    /// The sign-up page Edge described while this was built, cut down.
    fn sign_up() -> Tree {
        let nodes = vec![
            with(
                node(1, "RootWebArea", "Sign up page", &[2]),
                &[("url", json!("http://127.0.0.1:8000/"))],
            ),
            ignored(node(2, "none", "", &[3, 4, 20, 22, 30, 31, 32, 34, 36])),
            with(node(3, "heading", "Sign up", &[40]), &[("level", json!(1))]),
            node(40, "StaticText", "Sign up", &[41]),
            node(41, "InlineTextBox", "Sign up", &[]),
            node(4, "form", "", &[5, 8, 12]),
            node(5, "LabelText", "", &[6, 7]),
            node(6, "StaticText", "Name ", &[]),
            valued(
                with(
                    node(7, "textbox", "Name", &[70]),
                    &[focusable(), ("editable", json!("plaintext"))],
                ),
                "old",
            ),
            node(70, "generic", "", &[71]),
            node(71, "StaticText", "old", &[]),
            valued(
                with(
                    node(8, "combobox", "", &[9]),
                    &[focusable(), ("expanded", json!(false))],
                ),
                "Red",
            ),
            node(9, "MenuListPopup", "", &[10, 11]),
            with(
                node(10, "option", "Red", &[]),
                &[focusable(), ("selected", json!(true))],
            ),
            with(node(11, "option", "Green", &[]), &[focusable()]),
            with(
                node(12, "button", "Send", &[13]),
                &[focusable(), ("invalid", json!("false"))],
            ),
            node(13, "StaticText", "Send", &[]),
            node(20, "paragraph", "", &[21]),
            node(21, "StaticText", "Nothing yet", &[]),
            node(22, "paragraph", "", &[23, 24, 25, 26, 28, 29]),
            node(23, "StaticText", "Some ", &[]),
            node(24, "StaticText", "bold", &[]),
            node(25, "StaticText", " text and a ", &[]),
            with(node(26, "link", "Next page", &[27]), &[focusable()]),
            node(27, "StaticText", "Next page", &[]),
            node(28, "StaticText", " and ", &[]),
            with(node(29, "link", "New tab", &[]), &[focusable()]),
            with(node(30, "button", "Ask", &[]), &[focusable()]),
            with(
                node(31, "alert", "", &[33]),
                &[("live", json!("assertive"))],
            ),
            node(33, "StaticText", "Heads up", &[]),
            node(32, "generic", "", &[35]),
            node(35, "StaticText", "Plain div", &[]),
            with(
                node(34, "checkbox", "Remember me", &[]),
                &[("checked", json!("true")), focusable()],
            ),
            ignored(node(36, "generic", "", &[37])),
            with(
                node(37, "heading", "Inside a wrapper", &[38]),
                &[("level", json!(2))],
            ),
            with(node(38, "link", "a link in a heading", &[]), &[focusable()]),
        ];
        serde_json::from_value::<Tree>(json!({"nodes": nodes})).unwrap()
    }

    #[test]
    fn a_page_is_listed_with_a_ref_on_every_line() {
        let mut refs = Refs::default();
        let page = page(&sign_up(), false, &mut refs, None, HashMap::new());
        assert_eq!(page.named(), "Sign up page — http://127.0.0.1:8000/");
        assert_eq!(
            page.text(),
            [
                "- heading \"Sign up\" [e1] level 1",
                "- form [e2]",
                "  - textbox \"Name\" = \"old\" [e3]",
                "  - combobox = \"Red\" [e4] collapsed",
                "  - button \"Send\" [e5]",
                "- link \"Next page\" [e6]",
                "- link \"New tab\" [e7]",
                "- button \"Ask\" [e8]",
                "- alert [e9]",
                "  - \"Heads up\" [e10]",
                "- checkbox \"Remember me\" [e11] checked",
                "- heading \"Inside a wrapper\" [e12] level 2",
                "  - link \"a link in a heading\" [e13]",
            ]
            .join("\n")
        );
        // A point anywhere in an element finds its line: the text inside a
        // button, the box inside a text field.
        assert_eq!(page.line_of(13), Some(4));
        assert_eq!(page.line_of(71), Some(2));
        assert_eq!(page.line_of(999), None);
    }

    #[test]
    fn the_full_listing_adds_the_words_between() {
        let mut refs = Refs::default();
        page(&sign_up(), false, &mut refs, None, HashMap::new());
        let full = page(&sign_up(), true, &mut refs, None, HashMap::new());
        let text = full.text();
        // What was listed keeps its ref, and the words between are new lines.
        assert!(
            text.contains(
                "  - \"Name\" [e14]
  - textbox \"Name\""
            ),
            "{text}"
        );
        assert!(text.contains("- \"Nothing yet\" [e15]"), "{text}");
        // A sentence with links in it is one line, its links inline.
        assert!(
            text.contains("- \"Some bold text and a [Next page](e6) and [New tab](e7)\" [e16]"),
            "{text}"
        );
        assert!(text.contains("- \"Plain div\" [e17]"), "{text}");
        // A phrase running across a link is still found.
        let (found, note) = matching(&full.lines, "text and a next page");
        assert!(note.starts_with("1 of"), "{note}");
        assert!(found.starts_with("- \"Some bold"), "{found}");
        // A point on a link in the sentence finds the sentence.
        assert_eq!(full.line_of(27), full.line_of(23));
        // An element's own label and contents are not said twice.
        assert!(!text.contains("- \"Send\""), "{text}");
        assert!(!text.contains("- \"old\""), "{text}");
        assert!(!text.contains("Green"), "{text}");
    }

    fn frame(foreign: bool, session: Option<&str>) -> Arc<Frame> {
        Arc::new(Frame {
            id: "F1".into(),
            loader: "L1".into(),
            origin: "https://pay.example".into(),
            foreign,
            session: session.map(str::to_owned),
            path: Vec::new(),
        })
    }

    #[test]
    fn a_frame_is_listed_where_it_is_shown_under_a_header_the_page_cannot_write() {
        // The frame's field has the number of the page's heading: they are
        // two processes' nodes.
        let pay = json!({"nodes": [
            node(1, "RootWebArea", "Pay", &[3]),
            node(3, "textbox", "Card number", &[]),
        ]});
        let shop = json!({"nodes": [
            node(1, "RootWebArea", "Shop", &[4, 3, 9]),
            node(4, "button", "frame https://pay.example (same origin)", &[]),
            node(3, "heading", "Shop", &[]),
            node(9, "Iframe", "", &[]),
        ]});
        let (pay, shop): (Tree, Tree) = (
            serde_json::from_value(pay).unwrap(),
            serde_json::from_value(shop).unwrap(),
        );
        let mut refs = Refs::default();
        let paying = frame(true, Some("S1"));
        let inner = page(&pay, false, &mut refs, Some(&paying), HashMap::new()).lines;
        let inner = HashMap::from([(9, framed(&paying, inner))]);
        let listed = page(&shop, false, &mut refs, None, inner);
        assert_eq!(
            listed.text(),
            "- button \"frame https://pay.example (same origin)\" [e2]\n\
             - heading \"Shop\" [e3]\n\
             frame https://pay.example (cross-origin)\n\
             \x20 - textbox \"Card number\" [e1]"
        );
        let (within, node) = refs.place(1).unwrap();
        assert_eq!((within.map(|f| f.id.as_str()), node), (Some("F1"), 3));
        assert_eq!(
            refs.place(3).map(|(within, node)| (within.is_none(), node)),
            Some((true, 3))
        );
        assert_eq!(refs.name_for(None, 3), Some("heading \"Shop\""));
        assert_eq!(
            refs.name_for(Some(&paying), 3),
            Some("textbox \"Card number\"")
        );
        // Its nodes are another process's, so no point on the page finds it.
        assert_eq!(listed.line_of(3), Some(1));
        // The same frame after loading another page has new refs.
        let moved = Arc::new(Frame {
            loader: "L2".into(),
            ..frame(true, Some("S1")).as_ref().clone()
        });
        page(&pay, false, &mut refs, Some(&moved), HashMap::new());
        assert_eq!(
            refs.place(4)
                .map(|(within, _)| within.map(|f| f.loader.clone())),
            Some(Some("L2".into()))
        );
    }

    #[test]
    fn a_long_frame_is_cut_and_only_one_in_the_pages_process_keeps_its_nodes() {
        let lines = || {
            (0..250).map(|n| Line {
                text: format!("- button \"{n}\" [e{n}]"),
                plain: None,
                nodes: vec![n],
            })
        };
        let near = framed(&frame(false, None), lines().collect());
        assert_eq!(near.len(), 1 + FRAME_LINES + 1);
        assert_eq!(near[0].text, "frame https://pay.example (same origin)");
        assert_eq!(near[1].text, "  - button \"0\" [e0]");
        assert_eq!(near[1].nodes, [0]);
        assert_eq!(
            near[201].text,
            "  (50 more lines of this frame are not listed)"
        );
        let apart = framed(&frame(true, Some("S1")), lines().collect());
        assert!(apart.iter().all(|line| line.nodes.is_empty()));
    }

    #[test]
    fn a_ref_never_means_two_things() {
        let mut refs = Refs::default();
        page(&sign_up(), false, &mut refs, None, HashMap::new());
        assert_eq!(refs.place(5).map(|(_, node)| node), Some(12));
        assert_eq!(refs.label(5), Some("button \"Send\""));
        // The same page listed again keeps its refs.
        page(&sign_up(), false, &mut refs, None, HashMap::new());
        assert_eq!(refs.place(5).map(|(_, node)| node), Some(12));
        // After a new page, an old ref names nothing, even where the new
        // page's nodes have the same numbers as the old one's.
        refs.forget();
        assert_eq!(refs.place(5).map(|(_, node)| node), None);
        let again = page(&sign_up(), false, &mut refs, None, HashMap::new()).text();
        assert!(again.starts_with("- heading \"Sign up\" [e14]"), "{again}");
        assert_eq!(ref_number("e14"), Some(14));
        assert_eq!(ref_number("[e14]"), Some(14));
        assert_eq!(ref_number("14"), Some(14));
        assert_eq!(ref_number("button"), None);
    }

    #[test]
    fn a_long_page_is_shown_a_window_at_a_time() {
        let long: Vec<Line> = (0..1000)
            .map(|n| Line {
                text: format!("- link \"Result {n}\" [e{n}]"),
                plain: None,
                nodes: vec![n],
            })
            .collect();
        let (shown, note) = window(&long, 0);
        assert!(shown.len() <= PAGE_CHARS, "{}", shown.len());
        let last = shown.lines().count();
        let note = note.unwrap();
        assert!(note.contains(&format!("from: {}", last + 1)), "{note}");
        // Carrying on starts at the next line.
        let (next, _) = window(&long, last);
        assert!(
            next.starts_with(&format!("- link \"Result {last}\"")),
            "{next}"
        );
        // The end says there is nothing after it.
        let (_, end) = window(&long, 990);
        assert!(end.unwrap().contains("from: 1 starts at the top"));
        // A short page is shown whole, with nothing to say about it.
        assert_eq!(window(&long[..3], 0).1, None);
        // Finding keeps only what matches.
        // Each match comes with the lines after it, and a gap is marked.
        let (found, note) = matching(&long, "Result 99");
        assert!(note.starts_with("11 of 1000 lines"), "{note}");
        let found: Vec<&str> = found.lines().collect();
        assert_eq!(
            found[..4],
            [
                "- link \"Result 99\" [e99]",
                "- link \"Result 100\" [e100]",
                "- link \"Result 101\" [e101]",
                "…"
            ]
        );
        assert_eq!(found[4], "- link \"Result 990\" [e990]");
        assert_eq!(found.len(), 14, "{found:?}");
        assert!(matching(&long, "nowhere").1.starts_with("No line"));
    }

    #[test]
    fn a_long_page_is_read_from_where_its_content_starts() {
        let nodes = vec![
            node(1, "RootWebArea", "Article", &[2, 5]),
            node(2, "navigation", "", &[3, 4]),
            with(node(3, "link", "Home", &[]), &[focusable()]),
            with(node(4, "link", "About", &[]), &[focusable()]),
            node(5, "main", "", &[6]),
            with(
                node(6, "heading", "The article", &[]),
                &[("level", json!(1))],
            ),
        ];
        let tree = serde_json::from_value::<Tree>(json!({"nodes": nodes})).unwrap();
        let article = page(&tree, false, &mut Refs::default(), None, HashMap::new());
        assert_eq!(article.main, Some(3));
        assert!(article.lines[3].text.starts_with("- main"));
        assert_eq!(article.lines.len(), 5);
        let bare = page(
            &sign_up(),
            false,
            &mut Refs::default(),
            None,
            HashMap::new(),
        );
        assert_eq!(bare.main, None);
    }

    #[test]
    fn a_frames_element_gets_no_ref_and_one_left_out_is_said_to_be_there() {
        let shop = json!({"nodes": [
            node(1, "RootWebArea", "Shop", &[3, 9, 10]),
            node(3, "heading", "Shop", &[]),
            with(node(9, "Iframe", "Card payment", &[]), &[focusable()]),
            node(10, "Iframe", "", &[]),
        ]});
        let shop: Tree = serde_json::from_value(shop).unwrap();
        let listed = page(&shop, false, &mut Refs::default(), None, HashMap::new());
        assert_eq!(
            listed.text(),
            "- heading \"Shop\" [e1]\n- Iframe \"Card payment\" (its frame is not listed)"
        );
        // A point on it still finds its line.
        assert_eq!(listed.line_of(9), Some(1));
    }
}
