//! Plugins that run code: a `plugin.json` whose `main` is a JavaScript or a
//! Python file, so a plugin can do more than bring files without anyone
//! building Rust.
//!
//! What the code brings -- tools, actions, the events it hears, a panel, a
//! status, settings -- is declared in the manifest beside `main`, not found
//! by running it. So a list, a menu or the tools array is there without
//! starting anything, the tools array a session's cached prompt was built
//! with does not change when a plugin starts, and nothing runs for a plugin
//! until one of those is used. A plugin's code is someone else's program, so
//! none of it runs until the user has allowed the plugin, as its MCP servers
//! wait; one installed from somewhere is asked about again whenever its code
//! changes, while one the user put in the plugins folder themselves is not
//! asked again for their own edits.

#[cfg(feature = "js")]
mod js;
mod python;
mod running;
pub mod ui;

use crate::cancel::Cancel;
use crate::tools::{Tool, ToolContext};
use running::{Loaded, Waited};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex, PoisonError, RwLock};
use std::time::{Duration, Instant, SystemTime};

/// The most tools, actions and settings one plugin declares: room for a real
/// integration, and a bound on what a list or the tools array carries.
const MOST_TOOLS: usize = 32;
const MOST_ACTIONS: usize = 32;
const MOST_SETTINGS: usize = 16;
/// The largest schema one tool's parameters take, in bytes: every request
/// carries it.
const SCHEMA_BYTES: usize = 16_384;
/// The longest tool name a provider takes.
const TOOL_NAME_CHARS: usize = 64;
/// The code one plugin may hold, in bytes: far past a plugin written to be
/// read, and small enough to hash on every look at the list.
pub(crate) const CODE_BYTES: u64 = 4 * 1024 * 1024;
/// The files that count as a plugin's code, for its size and for what the
/// user allowed: what either runtime can load. `.pyc` because Python imports
/// one that sits alone in a folder.
const CODE_FILES: &[&str] = &["js", "mjs", "cjs", "py", "pyw", "pyc", "json"];
/// What a plugin may ask for beyond its own sandbox.
pub(crate) const PERMISSIONS: &[&str] = &[NETWORK, CONVERSATION];
/// Reaching the internet from JavaScript, through `letronna.fetch`.
const NETWORK: &str = "network";
/// Reading what is said: prompts, answers, and the arguments and output of
/// tool calls, in the events the plugin hears.
const CONVERSATION: &str = "conversation";

/// The events a plugin can ask to hear, each with its bit.
pub(crate) const EVENTS: &[(&str, u32)] = &[
    ("turn.start", TURN_START),
    ("turn.end", TURN_END),
    ("tool.before", TOOL_BEFORE),
    ("tool.after", TOOL_AFTER),
    ("approval.wait", APPROVAL_WAIT),
    ("prompt.submit", PROMPT_SUBMIT),
    ("turn.stop", TURN_STOP),
    ("tool.failed", TOOL_FAILED),
    ("model.reply", MODEL_REPLY),
];
pub(crate) const TURN_START: u32 = 1;
pub(crate) const TURN_END: u32 = 1 << 1;
pub(crate) const TOOL_BEFORE: u32 = 1 << 2;
pub(crate) const TOOL_AFTER: u32 = 1 << 3;
pub(crate) const APPROVAL_WAIT: u32 = 1 << 4;
pub(crate) const PROMPT_SUBMIT: u32 = 1 << 5;
pub(crate) const TURN_STOP: u32 = 1 << 6;
pub(crate) const TOOL_FAILED: u32 = 1 << 7;
pub(crate) const MODEL_REPLY: u32 = 1 << 8;

/// What a plugin.json says about the code it runs: its file, when it starts,
/// what it may reach, and what it brings. A plugin without `main` has none.
#[derive(Deserialize, Serialize, Default, Clone, Debug, PartialEq)]
pub(crate) struct Spec {
    #[serde(default)]
    main: String,
    #[serde(default)]
    start: Start,
    #[serde(default)]
    permissions: Vec<String>,
    #[serde(default)]
    contributes: Contributes,
}

/// When a plugin's code starts.
#[derive(Deserialize, Serialize, Default, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum Start {
    /// The first time something it brings is used.
    #[default]
    OnUse,
    /// With Letronna, for a status that shows from the start.
    Launch,
}

/// What a plugin's code brings, as its manifest declares it.
#[derive(Deserialize, Serialize, Default, Clone, Debug, PartialEq)]
#[serde(default)]
pub struct Contributes {
    pub tools: Vec<ToolSpec>,
    pub actions: Vec<ActionSpec>,
    pub events: Vec<String>,
    pub panel: Option<PanelSpec>,
    pub status: bool,
    pub settings: Vec<Setting>,
}

/// A tool the model can call, run by the plugin's code.
#[derive(Deserialize, Serialize, Clone, Debug, PartialEq)]
pub struct ToolSpec {
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default = "no_parameters")]
    pub parameters: Value,
    /// It only looks: with `openWorld` false and not destructive, it runs
    /// without a card, as a local MCP server's such tool does.
    #[serde(default, rename = "readOnly")]
    pub read_only: bool,
    /// It reaches past this computer. Unsaid, a Python plugin's tool does,
    /// as an MCP server's does, and a JavaScript one's does when the plugin
    /// may reach the network.
    #[serde(default, rename = "openWorld", skip_serializing_if = "Option::is_none")]
    pub open_world: Option<bool>,
    #[serde(default)]
    pub destructive: bool,
    /// Seconds one call may take; `TOOL_WAIT` when unsaid.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout: Option<u64>,
}

fn no_parameters() -> Value {
    serde_json::json!({"type": "object", "properties": {}})
}

/// A slash command that runs the plugin's code rather than filling the box.
#[derive(Deserialize, Serialize, Clone, Debug, PartialEq)]
pub struct ActionSpec {
    pub name: String,
    #[serde(default)]
    pub description: String,
    /// What to type after it.
    #[serde(default)]
    pub hint: String,
}

/// A tab of the window's side panel that the plugin draws.
#[derive(Deserialize, Serialize, Clone, Debug, PartialEq)]
pub struct PanelSpec {
    #[serde(default)]
    pub title: String,
}

/// One of a plugin's settings, which Letronna keeps and both front ends
/// show, so they can be changed without the plugin running.
#[derive(Deserialize, Serialize, Clone, Debug, PartialEq)]
pub struct Setting {
    pub key: String,
    #[serde(default)]
    pub label: String,
    #[serde(default, rename = "type")]
    pub kind: SettingKind,
    /// The values a `choice` takes.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub choices: Vec<String>,
    #[serde(default)]
    pub default: Value,
}

/// How a setting is changed.
#[derive(Deserialize, Serialize, Default, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum SettingKind {
    #[default]
    Switch,
    Text,
    Choice,
}

/// The language a plugin's code is in, from its `main` file's extension.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Runtime {
    JavaScript,
    Python,
}

impl Runtime {
    pub fn name(self) -> &'static str {
        match self {
            Runtime::JavaScript => "JavaScript",
            Runtime::Python => "Python",
        }
    }
}

/// A plugin's code as the lists show it and the user allows it.
#[derive(Clone, Debug, PartialEq)]
pub struct Code {
    pub runtime: Runtime,
    /// Its file, `/`-separated inside the plugin.
    pub main: String,
    pub start: Start,
    /// It may reach the internet (from JavaScript; Python always can).
    pub network: bool,
    /// It hears what is said, not only that something happened.
    pub conversation: bool,
    pub contributes: Contributes,
}

impl Code {
    /// The events it hears, as bits.
    pub(crate) fn events(&self) -> u32 {
        self.contributes
            .events
            .iter()
            .filter_map(|name| EVENTS.iter().find(|(known, _)| known == name))
            .fold(0, |bits, (_, bit)| bits | bit)
    }

    /// What allowing it lets happen, in a sentence for the user.
    pub fn consent(&self) -> String {
        let mut reach = Vec::new();
        if self.network && self.runtime == Runtime::JavaScript {
            reach.push("reach the internet");
        }
        if self.conversation {
            reach.push("read your conversation");
        }
        let reach = match reach.as_slice() {
            [] => String::new(),
            [one] => format!("; it can {one}"),
            [one, two] => format!("; it can {one} and {two}"),
            _ => unreachable!(),
        };
        match self.runtime {
            Runtime::JavaScript => format!(
                "Runs {} in Letronna's JavaScript sandbox{reach}.",
                self.main
            ),
            Runtime::Python => format!(
                "Runs {} as a Python program with your account's access to this computer{reach}.",
                self.main
            ),
        }
    }

    /// What it brings, one short phrase each, for a list.
    pub fn brings(&self) -> Vec<String> {
        let c = &self.contributes;
        let mut brings = Vec::new();
        let named = |what: &str, names: Vec<&str>| match names.len() {
            0 => None,
            _ => Some(format!("{what}: {}", names.join(", "))),
        };
        brings.extend(named(
            "tools",
            c.tools.iter().map(|tool| tool.name.as_str()).collect(),
        ));
        brings.extend(named(
            "commands",
            c.actions
                .iter()
                .map(|action| action.name.as_str())
                .collect(),
        ));
        brings.extend(named(
            "hears",
            c.events.iter().map(String::as_str).collect(),
        ));
        if let Some(panel) = &c.panel {
            brings.push(format!("a panel, {}", panel.title));
        }
        if c.status {
            brings.push("a status".into());
        }
        if !c.settings.is_empty() {
            brings.push(format!("{} settings", c.settings.len()));
        }
        brings
    }
}

/// The name the model calls a plugin's tool by: its own name under the
/// plugin's, so no plugin's tool can take a built-in's or an MCP server's.
pub(crate) fn tool_name(plugin: &str, tool: &str) -> String {
    format!("plugin__{plugin}__{tool}")
}

/// `spec` checked against the plugin `plugin` in `folder`: `None` when it
/// runs no code, or why it cannot be used, in a sentence.
pub(super) fn checked(plugin: &str, folder: &Path, spec: &Spec) -> Result<Option<Code>, String> {
    let manifest = super::MANIFEST;
    if spec.main.trim().is_empty() {
        if spec.contributes != Contributes::default() || !spec.permissions.is_empty() {
            return Err(format!(
                "{manifest} lists what code would bring, but names no main file to run."
            ));
        }
        return Ok(None);
    }
    let main = super::inside(&spec.main).map_err(|why| format!("Its main file: {why}"))?;
    let runtime = match main.rsplit_once('.').map(|(_, ext)| ext) {
        Some("js" | "mjs" | "cjs") => Runtime::JavaScript,
        Some("py") => Runtime::Python,
        _ => {
            return Err(format!(
                "Its main file is {main}; a plugin's code is a .js or a .py file."
            ));
        }
    };
    if !folder.join(&main).is_file() {
        return Err(format!("Its main file, {main}, is not there."));
    }
    #[cfg(not(feature = "js"))]
    if runtime == Runtime::JavaScript {
        return Err(
            "This build of Letronna leaves the JavaScript engine out, and this plugin needs it."
                .into(),
        );
    }
    for permission in &spec.permissions {
        if !PERMISSIONS.contains(&permission.as_str()) {
            return Err(format!(
                "{manifest} asks for \"{}\"; a plugin can ask for {}.",
                crate::skills::printable(permission),
                PERMISSIONS.join(" and ")
            ));
        }
    }
    let c = &spec.contributes;
    counted("tools", c.tools.len(), MOST_TOOLS)?;
    counted("commands", c.actions.len(), MOST_ACTIONS)?;
    counted("settings", c.settings.len(), MOST_SETTINGS)?;
    named("tool", c.tools.iter().map(|tool| tool.name.as_str()))?;
    named("command", c.actions.iter().map(|a| a.name.as_str()))?;
    named("setting", c.settings.iter().map(|s| s.key.as_str()))?;
    for tool in &c.tools {
        let full = tool_name(plugin, &tool.name);
        if full.chars().count() > TOOL_NAME_CHARS {
            return Err(format!(
                "Its tool {} would be called {full}, longer than the {TOOL_NAME_CHARS} \
                 characters a model's tool name may be; give it or the plugin a shorter name.",
                tool.name
            ));
        }
        if tool.description.trim().is_empty() {
            return Err(format!(
                "Its tool {} has no description, which is all the model has to go on.",
                tool.name
            ));
        }
        if tool.parameters["type"] != "object" {
            return Err(format!(
                "Its tool {}'s parameters are not a JSON schema of type object.",
                tool.name
            ));
        }
        if tool.parameters.to_string().len() > SCHEMA_BYTES {
            return Err(format!(
                "Its tool {}'s parameters are over {SCHEMA_BYTES} bytes, and every request \
                 would carry them.",
                tool.name
            ));
        }
    }
    for event in &c.events {
        if !EVENTS.iter().any(|(known, _)| known == event) {
            let known: Vec<&str> = EVENTS.iter().map(|(name, _)| *name).collect();
            return Err(format!(
                "{manifest} asks to hear \"{}\"; the events are {}.",
                crate::skills::printable(event),
                known.join(", ")
            ));
        }
    }
    for setting in &c.settings {
        if setting.kind == SettingKind::Choice && setting.choices.is_empty() {
            return Err(format!(
                "Its setting {} is a choice with nothing to choose from.",
                setting.key
            ));
        }
    }
    let mut contributes = c.clone();
    for setting in &mut contributes.settings {
        if setting.label.trim().is_empty() {
            setting.label = setting.key.clone();
        }
    }
    if let Some(panel) = &mut contributes.panel
        && panel.title.trim().is_empty()
    {
        panel.title = plugin.to_owned();
    }
    let asked = |permission: &str| spec.permissions.iter().any(|p| p == permission);
    Ok(Some(Code {
        runtime,
        main,
        start: spec.start,
        network: asked(NETWORK),
        conversation: asked(CONVERSATION),
        contributes,
    }))
}

/// Refuses more than `most` of `what`.
fn counted(what: &str, count: usize, most: usize) -> Result<(), String> {
    match count > most {
        true => Err(format!(
            "{} declares {count} {what}; a plugin brings at most {most}.",
            super::MANIFEST
        )),
        false => Ok(()),
    }
}

/// Refuses a name that is not letters, digits, `-` and `_`, or that comes
/// twice.
fn named<'a>(what: &str, names: impl Iterator<Item = &'a str>) -> Result<(), String> {
    let mut seen: Vec<&str> = Vec::new();
    for name in names {
        let plain = name.starts_with(|c: char| c.is_ascii_alphanumeric())
            && name
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'))
            && name.chars().count() <= super::NAME_CHARS;
        if !plain {
            return Err(format!(
                "A {what} is called \"{}\"; a name is up to {} letters, digits, - and _.",
                crate::skills::printable(name),
                super::NAME_CHARS
            ));
        }
        if seen.iter().any(|known| known.eq_ignore_ascii_case(name)) {
            return Err(format!("Two of its {what}s are called {name}."));
        }
        seen.push(name);
    }
    Ok(())
}

/// Whether the file at `relative` is code, as either runtime would load it.
pub(super) fn is_code(relative: &str) -> bool {
    relative
        .rsplit_once('.')
        .is_some_and(|(_, ext)| CODE_FILES.contains(&ext.to_ascii_lowercase().as_str()))
}

/// Adds every code file in `folder` to `hash`, by name and content in name
/// order, so a change to any of them, or one added or taken away, is a new
/// consent. One that cannot be read hashes as that, which no consent given
/// before matches.
pub(super) fn digest(folder: &Path, hash: &mut ring::digest::Context) {
    let mut files = Vec::new();
    if let Err(why) = super::walk(folder, folder, 0, &mut files) {
        hash.update(why.as_bytes());
        return;
    }
    files.retain(|(relative, ..)| is_code(relative));
    files.sort_by(|a, b| a.0.cmp(&b.0));
    let mut left = CODE_BYTES;
    for (relative, size, path) in files {
        hash.update(relative.as_bytes());
        hash.update(&size.to_le_bytes());
        let mut bytes = Vec::new();
        let read = std::fs::File::open(&path).and_then(|file| {
            use std::io::Read;
            file.take(left).read_to_end(&mut bytes)
        });
        match read {
            Ok(_) => hash.update(&bytes),
            Err(why) => hash.update(why.to_string().as_bytes()),
        }
        left = left.saturating_sub(bytes.len() as u64);
    }
}

/// Where each plugin keeps its settings, storage and log, in the data folder.
const DATA_DIR: &str = "plugin-data";
/// How long the plugins hearing `tool.before` or `tool.after` have to
/// answer between them, and how long when one of them had to start first.
const HOOK_WAIT: Duration = Duration::from_secs(2);
const COLD_HOOK_WAIT: Duration = Duration::from_secs(10);
/// How long the plugins hearing `turn.stop` have to look at the work: room
/// for a test suite to run. The user's stop ends the wait sooner.
const STOP_WAIT: Duration = Duration::from_secs(300);
/// How long a tool call or an action may take unless the plugin says.
const TOOL_WAIT: Duration = Duration::from_secs(120);
const LONGEST_TOOL_WAIT: Duration = Duration::from_secs(600);
/// How long an action may take.
const ACTION_WAIT: Duration = Duration::from_secs(120);

/// The plugins allowed to run code now, and the tools they bring.
struct Registry {
    plugins: Vec<Arc<Loaded>>,
    tools: Vec<Arc<dyn Tool>>,
    /// What the plugins' files looked like when this was read, so a change
    /// made elsewhere -- in the other front end -- is seen at a turn's start.
    stamp: Vec<(PathBuf, Option<(SystemTime, u64)>)>,
}

static REGISTRY: RwLock<Option<Arc<Registry>>> = RwLock::new(None);
/// One reload at a time, so two cannot each start their own copy of a
/// plugin.
static RELOADING: Mutex<()> = Mutex::new(());
/// The events any plugin hears, as bits: every place an event happens looks
/// here first, so with no plugin hearing it, it costs one load and builds
/// nothing.
static WANTS: AtomicU32 = AtomicU32::new(0);
/// Set once a front end has started, so the plugins that run with Letronna
/// start; a test or a one-off run reading the list starts nothing.
static LAUNCHED: AtomicBool = AtomicBool::new(false);
/// The conversations a turn has started in since Letronna did.
static SEEN: Mutex<Option<HashSet<u64>>> = Mutex::new(None);
/// What each conversation's turn has used so far, for `turn.end`; kept only
/// while a plugin hears that.
static TALLIES: Mutex<Option<HashMap<u64, Tally>>> = Mutex::new(None);

/// What one turn has used: its time, its model's requests and tokens, and
/// its tool calls.
struct Tally {
    began: Instant,
    requests: u64,
    input: u64,
    output: u64,
    cached: u64,
    cost: Option<f64>,
    calls: u64,
    failed: u64,
}

impl Tally {
    fn new() -> Self {
        Self {
            began: Instant::now(),
            requests: 0,
            input: 0,
            output: 0,
            cached: 0,
            cost: None,
            calls: 0,
            failed: 0,
        }
    }

    /// Counts one of the model's replies, as `model.reply` tells it.
    fn replied(&mut self, usage: &Value) {
        let count = |key: &str| usage[key].as_u64().unwrap_or(0);
        self.requests += 1;
        self.input += count("input_tokens");
        self.output += count("output_tokens");
        self.cached += count("cached_tokens");
        if let Some(cost) = usage["cost"].as_f64() {
            *self.cost.get_or_insert(0.0) += cost;
        }
    }

    /// What `turn.end` says the turn used.
    fn told(&self) -> Value {
        json!({
            "ms": self.began.elapsed().as_millis() as u64,
            "requests": self.requests,
            "input_tokens": self.input,
            "output_tokens": self.output,
            "cached_tokens": self.cached,
            "cost": self.cost,
            "tool_calls": self.calls,
            "tool_failures": self.failed,
        })
    }
}

/// Adds to `conversation`'s turn's tally, when a plugin hears `turn.end`.
fn tally(conversation: u64, add: impl FnOnce(&mut Tally)) {
    if !wants(TURN_END) {
        return;
    }
    let mut tallies = lock(&TALLIES);
    let tally = tallies
        .get_or_insert_with(HashMap::new)
        .entry(conversation)
        .or_insert_with(Tally::new);
    add(tally);
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The data folder the plugins are read from.
#[cfg(not(test))]
fn data() -> PathBuf {
    crate::chat::data_dir()
}

/// In tests, a folder no plugin is in, so no test runs the plugins of
/// whoever runs the tests; the tests build registries of their own.
#[cfg(test)]
fn data() -> PathBuf {
    std::env::temp_dir().join(format!("letronna-no-plugins-{}", std::process::id()))
}

/// The registry, read now if it never was.
fn current() -> Arc<Registry> {
    let read = REGISTRY.read().unwrap_or_else(PoisonError::into_inner);
    if let Some(registry) = read.as_ref() {
        return registry.clone();
    }
    drop(read);
    reload_in(&data())
}

/// Starts the plugins that run with Letronna, off the caller's thread. A
/// front end calls this once it is up.
pub fn init() {
    LAUNCHED.store(true, Ordering::SeqCst);
    let _ = std::thread::Builder::new()
        .name("plugins".into())
        .spawn(|| {
            for plugin in &current().plugins {
                if plugin.code.start == Start::Launch {
                    plugin.wake();
                }
            }
        });
}

/// Reads the plugins again after one changed: one switched off, taken out
/// or changed stops, and the rest keep running.
pub fn reload() {
    reload_in(&data());
}

fn reload_in(data: &Path) -> Arc<Registry> {
    let _one = lock(&RELOADING);
    let before = REGISTRY
        .read()
        .unwrap_or_else(PoisonError::into_inner)
        .clone();
    let registry = Arc::new(build(data, before.as_deref()));
    for gone in before.iter().flat_map(|before| &before.plugins) {
        if !registry.plugins.iter().any(|kept| Arc::ptr_eq(kept, gone)) {
            gone.retire();
        }
    }
    *REGISTRY.write().unwrap_or_else(PoisonError::into_inner) = Some(registry.clone());
    WANTS.store(registry.wants(), Ordering::Relaxed);
    if LAUNCHED.load(Ordering::SeqCst) {
        for plugin in &registry.plugins {
            if plugin.code.start == Start::Launch {
                plugin.wake();
            }
        }
    }
    registry
}

/// The plugins in `data` allowed to run code, each kept from `before` when
/// nothing about it changed, so it goes on running.
fn build(data: &Path, before: Option<&Registry>) -> Registry {
    let stamp = stamp(data);
    let mut plugins = Vec::new();
    for (name, folder, code) in super::coded_in(data) {
        let kept = before.and_then(|before| {
            before
                .plugins
                .iter()
                .find(|plugin| {
                    plugin.name == name && plugin.folder == folder && plugin.code == code
                })
                .cloned()
        });
        let own = data.join(DATA_DIR).join(&name);
        plugins.push(kept.unwrap_or_else(|| Loaded::new(name, folder, own, code)));
    }
    let tools = plugins.iter().flat_map(CodeTool::of).collect();
    Registry {
        plugins,
        tools,
        stamp,
    }
}

impl Registry {
    /// The events any of its plugins hears, as bits.
    fn wants(&self) -> u32 {
        self.plugins
            .iter()
            .fold(0, |wants, plugin| wants | plugin.events)
    }

    fn find(&self, name: &str) -> Result<&Arc<Loaded>, String> {
        self.plugins
            .iter()
            .find(|plugin| plugin.name == name)
            .ok_or_else(|| {
                format!("No plugin called {name} runs code, or it is off or not allowed.")
            })
    }
}

/// The modified time and length of `plugins.json` and of every plugin's
/// manifest, which is what a switch, a consent or an update changes.
fn stamp(data: &Path) -> Vec<(PathBuf, Option<(SystemTime, u64)>)> {
    let look = |path: PathBuf| {
        let seen = std::fs::metadata(&path)
            .ok()
            .and_then(|meta| Some((meta.modified().ok()?, meta.len())));
        (path, seen)
    };
    std::iter::once(look(data.join(super::STATE_FILE)))
        .chain(
            super::plugin_folders(data)
                .into_iter()
                .map(|folder| look(folder.join(super::MANIFEST))),
        )
        .collect()
}

/// Reads the plugins again if their files changed since they were read.
fn reread() {
    let data = data();
    if current().stamp != stamp(&data) {
        reload_in(&data);
    }
}

/// Stops one plugin's code, as before its folder moves; it starts again
/// from whatever is in place when next needed.
pub(crate) fn stop(name: &str) {
    for plugin in current()
        .plugins
        .iter()
        .filter(|plugin| plugin.name == name)
    {
        plugin.stop();
    }
}

/// Stops one plugin's code and forgets its failures, for an author who
/// changed it; it starts again on its next use, or now if it runs with
/// Letronna.
pub fn restart(name: &str) -> Result<(), String> {
    let registry = current();
    let plugin = registry.find(name)?;
    plugin.stop();
    plugin.forgive();
    if plugin.code.start == Start::Launch && LAUNCHED.load(Ordering::SeqCst) {
        plugin.wake();
    }
    Ok(())
}

/// Stops every plugin's code, as Letronna quits.
pub fn stop_all() {
    let registry = REGISTRY
        .read()
        .unwrap_or_else(PoisonError::into_inner)
        .clone();
    for plugin in registry.iter().flat_map(|registry| &registry.plugins) {
        plugin.stop();
    }
}

/// The tools the plugins' code brings.
pub(crate) fn tools() -> Vec<Arc<dyn Tool>> {
    current().tools.clone()
}

/// The names of the tools the plugins' code brings.
pub(crate) fn tool_names() -> Vec<String> {
    current()
        .tools
        .iter()
        .map(|tool| tool.name().to_owned())
        .collect()
}

/// Whether any plugin hears the event `bit`.
fn wants(bit: u32) -> bool {
    WANTS.load(Ordering::Relaxed) & bit != 0
}

/// Queues the event `name` for every plugin that hears `bit`, starting any
/// that has not started. `bare` is what any of them is told; `said`, what
/// was said, is added for those that may read the conversation.
fn emit(registry: &Registry, bit: u32, name: &str, bare: Value, said: impl Fn() -> Value) {
    let mut words = None;
    for plugin in registry
        .plugins
        .iter()
        .filter(|plugin| plugin.events & bit != 0)
    {
        let mut data = bare.clone();
        if plugin.code.conversation {
            merge(&mut data, words.get_or_insert_with(&said));
        }
        plugin.post(json!({"type": "event", "event": name, "data": data}));
        plugin.wake();
    }
}

fn merge(into: &mut Value, from: &Value) {
    if let (Some(into), Some(from)) = (into.as_object_mut(), from.as_object()) {
        into.extend(from.iter().map(|(key, value)| (key.clone(), value.clone())));
    }
}

/// A turn starts in `conversation`. The plugins are read again if their
/// files changed, and those that look at tool calls are started now, so the
/// turn's first call does not wait on them.
pub(crate) fn turn_started(conversation: u64) {
    reread();
    if wants(TOOL_BEFORE | TOOL_AFTER) {
        for plugin in &current().plugins {
            if plugin.events & (TOOL_BEFORE | TOOL_AFTER) != 0 {
                plugin.wake();
            }
        }
    }
    if wants(TURN_END) {
        (lock(&TALLIES).get_or_insert_with(HashMap::new)).insert(conversation, Tally::new());
    }
    if !wants(TURN_START) {
        return;
    }
    let first = lock(&SEEN)
        .get_or_insert_with(HashSet::new)
        .insert(conversation);
    let bare = json!({"conversation": conversation, "first": first});
    emit(&current(), TURN_START, "turn.start", bare, || json!({}));
}

/// A turn ended in `conversation`, working in `workspace`, with `answer`,
/// the model's last words, or `None` when the user stopped it. It carries
/// what the turn used.
pub fn turn_ended(conversation: u64, workspace: Option<&Path>, answer: Option<&str>) {
    let used = (lock(&TALLIES).as_mut()).and_then(|tallies| tallies.remove(&conversation));
    if !wants(TURN_END) {
        return;
    }
    let mut bare = json!({
        "conversation": conversation,
        "workspace": workspace,
        "stopped": answer.is_none(),
    });
    merge(&mut bare, &used.unwrap_or_else(Tally::new).told());
    emit(
        &current(),
        TURN_END,
        "turn.end",
        bare,
        || json!({"answer": answer}),
    );
}

/// Whether any plugin hears `prompt.submit`, so a front end waits for what
/// they add before the turn starts. With none, nothing waits.
pub fn hears_prompts() -> bool {
    wants(PROMPT_SUBMIT)
}

/// The user sent `text` in `conversation`, working in `workspace`: what the
/// plugins hearing `prompt.submit` add for the model to read with it, as a
/// notice to go just before it, or `None`. It waits for them, up to
/// `HOOK_WAIT`, so a front end calls it off its window's thread.
pub fn prompt_context(
    conversation: u64,
    workspace: Option<&Path>,
    text: &str,
    cancel: &Cancel,
) -> Option<crate::chat::Message> {
    if !wants(PROMPT_SUBMIT) {
        return None;
    }
    prompt_context_in(&current(), conversation, workspace, text, cancel)
}

fn prompt_context_in(
    registry: &Registry,
    conversation: u64,
    workspace: Option<&Path>,
    text: &str,
    cancel: &Cancel,
) -> Option<crate::chat::Message> {
    let bare = json!({"conversation": conversation, "workspace": workspace});
    let said = || json!({"text": text});
    let heard = ask_all(
        registry,
        PROMPT_SUBMIT,
        "prompt.submit",
        bare,
        said,
        HOOK_WAIT,
        cancel,
    );
    let ctx = ToolContext {
        conversation,
        ..Default::default()
    };
    let added: Vec<String> = heard
        .into_iter()
        .filter_map(|(plugin, waited)| match waited {
            Waited::Said(said) => {
                let note = said["note"]
                    .as_str()
                    .filter(|note| !note.trim().is_empty())?;
                let lead = format!("[The {} plugin adds:]", plugin.name);
                Some(crate::untrusted::quoted(&lead, note))
            }
            Waited::Stopped => None,
            other => {
                plugin.problem(&late(other, "prompt.submit", "the message went without it"));
                None
            }
        })
        .collect();
    if added.is_empty() {
        return None;
    }
    crate::taint::mark(&ctx);
    Some(crate::chat::Message {
        role: "user".into(),
        content: format!("{PROMPT_CONTEXT}\n\n{}", added.join("\n\n")),
        notice: true,
        ..Default::default()
    })
}

/// What heads what plugins add to a prompt, so the model reads it as theirs.
const PROMPT_CONTEXT: &str =
    "Plugins the user allowed added this for your reading of the user's next message:";

/// Whether any plugin hears `turn.stop`, so the front end asks before a
/// turn ends.
pub(crate) fn stops() -> bool {
    wants(TURN_STOP)
}

/// The model finished a turn with `last` as its reply: what the plugins
/// hearing `turn.stop` say is left to do, if any, as the words that send the
/// model back to it. `again` when the turn was sent back already. A plugin
/// may take `STOP_WAIT`, to run tests say, and the user's stop ends it.
pub(crate) fn turn_stop(ctx: &ToolContext, last: &str, again: bool) -> Option<String> {
    if !wants(TURN_STOP) {
        return None;
    }
    turn_stop_in(&current(), ctx, last, again)
}

fn turn_stop_in(registry: &Registry, ctx: &ToolContext, last: &str, again: bool) -> Option<String> {
    let bare = json!({
        "conversation": ctx.conversation,
        "workspace": ctx.workspace,
        "again": again,
    });
    let said = || json!({"answer": last});
    let heard = ask_all(
        registry,
        TURN_STOP,
        "turn.stop",
        bare,
        said,
        STOP_WAIT,
        &ctx.cancel,
    );
    for (plugin, waited) in heard {
        let said = match waited {
            Waited::Said(said) => said,
            Waited::Stopped => return None,
            other => {
                plugin.problem(&late(other, "turn.stop", "the turn ended"));
                continue;
            }
        };
        // A block and a note alike say what is left to do.
        let left = [&said["block"], &said["note"]]
            .into_iter()
            .filter_map(Value::as_str)
            .find(|left| !left.trim().is_empty());
        if let Some(left) = left {
            crate::taint::mark(ctx);
            let lead = format!("The {} plugin says the work is not done:", plugin.name);
            return Some(crate::untrusted::quoted(&lead, left));
        }
    }
    None
}

/// The model answered a request in `conversation` on `model`, with what it
/// cost: tallied for `turn.end`, and told to the plugins hearing
/// `model.reply`.
pub(crate) fn model_replied(conversation: u64, model: &str, usage: &Value) {
    if !wants(MODEL_REPLY | TURN_END) {
        return;
    }
    tally(conversation, |tally| tally.replied(usage));
    if !wants(MODEL_REPLY) {
        return;
    }
    let mut bare = json!({"conversation": conversation, "model": model});
    merge(&mut bare, usage);
    emit(&current(), MODEL_REPLY, "model.reply", bare, || json!({}));
}

/// A card for a call of `tool` waits for the user: a question when it is
/// `ask_user`, else an approval.
pub(crate) fn approval_waits(ctx: &ToolContext, tool: &str) {
    if !wants(APPROVAL_WAIT) {
        return;
    }
    let kind = match tool == crate::tools::ASK_USER {
        true => "question",
        false => "permission",
    };
    let bare = json!({"conversation": ctx.conversation, "tool": tool, "kind": kind});
    emit(&current(), APPROVAL_WAIT, "approval.wait", bare, || {
        json!({})
    });
}

/// Asks every plugin that hears `bit` about a call, all at once, and
/// gathers what each said by one deadline.
fn ask_all(
    registry: &Registry,
    bit: u32,
    name: &str,
    bare: Value,
    said: impl Fn() -> Value,
    wait: Duration,
    cancel: &Cancel,
) -> Vec<(Arc<Loaded>, Waited)> {
    let hearing: Vec<&Arc<Loaded>> = (registry.plugins.iter())
        .filter(|plugin| plugin.events & bit != 0)
        .collect();
    let cold = hearing.iter().any(|plugin| !plugin.running());
    let until = Instant::now() + if cold { wait.max(COLD_HOOK_WAIT) } else { wait };
    let mut words = None;
    let asked: Vec<_> = hearing
        .into_iter()
        .map(|plugin| {
            let mut data = bare.clone();
            if plugin.code.conversation {
                merge(&mut data, words.get_or_insert_with(&said));
            }
            let message = json!({"type": "hook", "event": name, "data": data});
            (plugin.clone(), plugin.request(message, Some(cancel)))
        })
        .collect();
    asked
        .into_iter()
        .map(|(plugin, pending)| {
            let waited = match pending {
                Ok(pending) => pending.wait(until),
                Err(why) => Waited::Failed(why),
            };
            (plugin, waited)
        })
        .collect()
}

/// Lets the plugins that hear `tool.before` look at a call the gate let
/// through: `Err` with why when one blocks it, else what they added for the
/// model. One that fails or is late is reported, and the call goes on: a
/// plugin can only add a block to what the gate allowed.
pub(crate) fn before(tool: &str, args: &Value, ctx: &ToolContext) -> Result<String, String> {
    if !wants(TOOL_BEFORE) {
        return Ok(String::new());
    }
    before_in(&current(), tool, args, ctx)
}

/// What every plugin hearing a call is told of it.
fn call(tool: &str, ctx: &ToolContext) -> Value {
    json!({"conversation": ctx.conversation, "workspace": ctx.workspace, "tool": tool})
}

fn before_in(
    registry: &Registry,
    tool: &str,
    args: &Value,
    ctx: &ToolContext,
) -> Result<String, String> {
    let bare = call(tool, ctx);
    let said = || json!({"args": args});
    let heard = ask_all(
        registry,
        TOOL_BEFORE,
        "tool.before",
        bare,
        said,
        HOOK_WAIT,
        &ctx.cancel,
    );
    let mut notes = String::new();
    for (plugin, waited) in heard {
        let said = match waited {
            Waited::Said(said) => said,
            Waited::Stopped => continue,
            other => {
                plugin.problem(&late(other, "tool.before", "the call went on"));
                continue;
            }
        };
        if let Some(why) = said["block"].as_str() {
            crate::taint::mark(ctx);
            return Err(crate::untrusted::quoted(
                &format!("The {} plugin blocked this call:", plugin.name),
                why,
            ));
        }
        notes.push_str(&noted(&plugin, &said, ctx));
    }
    Ok(notes)
}

/// Lets the plugins that hear `tool.after` look at a call that ran with
/// `output`: what they added for the model, or nothing.
/// Lets the plugins that hear `tool.after` look at a call that ran with
/// `output` in `took`: what they added for the model, or nothing.
pub(crate) fn after(
    tool: &str,
    args: &Value,
    output: &str,
    took: Duration,
    ctx: &ToolContext,
) -> String {
    tally(ctx.conversation, |tally| tally.calls += 1);
    if !wants(TOOL_AFTER) {
        return String::new();
    }
    let said = || json!({"args": args, "output": output});
    ran_in(&current(), TOOL_AFTER, "tool.after", tool, took, said, ctx)
}

/// Lets the plugins that hear `tool.failed` look at a call that ran and
/// failed with `error` in `took`: what they added for the model, such as
/// that it failed the same way before, or nothing.
pub(crate) fn failed(
    tool: &str,
    args: &Value,
    error: &str,
    took: Duration,
    ctx: &ToolContext,
) -> String {
    tally(ctx.conversation, |tally| {
        tally.calls += 1;
        tally.failed += 1;
    });
    if !wants(TOOL_FAILED) {
        return String::new();
    }
    let said = || json!({"args": args, "error": error});
    ran_in(
        &current(),
        TOOL_FAILED,
        "tool.failed",
        tool,
        took,
        said,
        ctx,
    )
}

/// Asks the plugins hearing `bit` about a call that ran, giving them how
/// long it took, and gathers their notes.
fn ran_in(
    registry: &Registry,
    bit: u32,
    event: &str,
    tool: &str,
    took: Duration,
    said: impl Fn() -> Value,
    ctx: &ToolContext,
) -> String {
    let mut bare = call(tool, ctx);
    bare["ms"] = (took.as_millis() as u64).into();
    let heard = ask_all(registry, bit, event, bare, said, HOOK_WAIT, &ctx.cancel);
    let mut notes = String::new();
    for (plugin, waited) in heard {
        match waited {
            Waited::Said(said) => notes.push_str(&noted(&plugin, &said, ctx)),
            Waited::Stopped => {}
            other => plugin.problem(&late(other, event, "it was left out")),
        }
    }
    notes
}

/// A plugin's note on a call, as the model gets it: fenced, as anyone may
/// have written the plugin.
fn noted(plugin: &Loaded, said: &Value, ctx: &ToolContext) -> String {
    let Some(note) = said["note"].as_str().filter(|note| !note.trim().is_empty()) else {
        return String::new();
    };
    crate::taint::mark(ctx);
    let lead = format!("[The {} plugin notes:]", plugin.name);
    format!("\n\n{}", crate::untrusted::quoted(&lead, note))
}

/// What the user is told of a plugin that did not answer about a call.
fn late(waited: Waited, event: &str, so: &str) -> String {
    match waited {
        Waited::Failed(why) => format!("could not hear {event} ({why}), so {so}."),
        _ => format!("did not answer {event} in time, so {so}."),
    }
}

/// A plugin's slash command, as the menus list it.
#[derive(Clone, Debug, PartialEq)]
pub struct Action {
    pub plugin: String,
    pub name: String,
    pub description: String,
    pub hint: String,
}

/// What an action gave back.
#[derive(Clone, Debug, PartialEq)]
pub enum Said {
    /// Shown to the user.
    Text(String),
    /// Put in the box for the user to read and send.
    Compose(String),
}

/// The plugins' actions, in plugin order.
pub fn actions() -> Vec<Action> {
    let registry = current();
    let mut actions = Vec::new();
    for plugin in &registry.plugins {
        for action in &plugin.code.contributes.actions {
            actions.push(Action {
                plugin: plugin.name.clone(),
                name: action.name.clone(),
                description: action.description.clone(),
                hint: action.hint.clone(),
            });
        }
    }
    actions
}

/// Runs the action `name` with what was typed after it, in `conversation`
/// and `workspace`, until `cancel` is set: what it gave back, or why not.
pub fn run_action(
    name: &str,
    args: &str,
    conversation: u64,
    workspace: Option<&Path>,
    cancel: &Cancel,
) -> Result<Said, String> {
    run_action_in(&current(), name, args, conversation, workspace, cancel)
}

fn run_action_in(
    registry: &Registry,
    name: &str,
    args: &str,
    conversation: u64,
    workspace: Option<&Path>,
    cancel: &Cancel,
) -> Result<Said, String> {
    let plugin = (registry.plugins.iter())
        .find(|plugin| (plugin.code.contributes.actions.iter()).any(|action| action.name == name))
        .ok_or_else(|| format!("No plugin has a command called {name}."))?;
    let message = json!({
        "type": "action",
        "name": name,
        "args": args,
        "ctx": {"conversation": conversation, "workspace": workspace},
    });
    let said = match plugin.ask(message, ACTION_WAIT, Some(cancel)) {
        Waited::Said(said) => said,
        Waited::Failed(why) => return Err(why),
        Waited::Stopped => return Err(format!("/{name} was stopped.")),
        Waited::Late => {
            return Err(plugin.said(&format!(
                "did not finish /{name} within {} s.",
                ACTION_WAIT.as_secs()
            )));
        }
    };
    if let Some(why) = said["error"].as_str() {
        return Err(plugin.said(&format!("could not run /{name}: {why}")));
    }
    Ok(match &said["ok"] {
        Value::Object(given) if given.contains_key("compose") => {
            Said::Compose(text_of(&given["compose"]))
        }
        given => Said::Text(text_of(given)),
    })
}

/// A value a plugin gave back, as text: a string as it is, anything else as
/// JSON.
fn text_of(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        Value::Null => String::new(),
        other => serde_json::to_string_pretty(other).unwrap_or_default(),
    }
}

/// The panels the plugins draw: each plugin's name, kept for the rest of
/// the run so a tab can hold it, and its panel's title.
pub fn panels() -> Vec<(&'static str, String)> {
    current()
        .plugins
        .iter()
        .filter_map(|plugin| {
            let panel = plugin.code.contributes.panel.as_ref()?;
            Some((crate::mcp::keep(plugin.name.clone()), panel.title.clone()))
        })
        .collect()
}

/// Whether a Python that plugins can run on is on this computer: `None`
/// until the first look, which this starts off the caller's thread.
pub fn python_found() -> Option<bool> {
    python::available()
}

/// The plugins that show a status, by name.
pub fn statuses() -> Vec<String> {
    current()
        .plugins
        .iter()
        .filter(|plugin| plugin.code.contributes.status)
        .map(|plugin| plugin.name.clone())
        .collect()
}

fn find(name: &str) -> Result<Arc<Loaded>, String> {
    current().find(name).cloned()
}

/// `plugin`'s panel was opened: it starts if need be and hears so.
pub fn panel_opened(plugin: &str) {
    if let Ok(plugin) = find(plugin) {
        plugin.post(json!({"type": "ui.open", "surface": "panel"}));
        plugin.wake();
    }
}

/// The user pressed, flipped or typed into the part `id` of `plugin`'s
/// panel, which now holds `value`.
pub fn panel_input(plugin: &str, id: &str, value: Value) {
    if let Ok(plugin) = find(plugin) {
        plugin.post(json!({"type": "ui", "surface": "panel", "id": id, "value": value}));
        plugin.wake();
    }
}

/// `plugin`'s settings, each with its value now. They are read without the
/// plugin running.
pub fn settings(plugin: &str) -> Result<Vec<(Setting, Value)>, String> {
    let plugin = find(plugin)?;
    let values = plugin.settings();
    Ok((plugin.code.contributes.settings.iter())
        .map(|setting| {
            let value = values.get(&setting.key).cloned().unwrap_or_default();
            (setting.clone(), value)
        })
        .collect())
}

/// Keeps `value` as `plugin`'s setting `key`, telling its code if it runs.
pub fn set_setting(plugin: &str, key: &str, value: Value) -> Result<(), String> {
    find(plugin)?.set(key, value)
}

/// A tool a plugin's code runs.
struct CodeTool {
    plugin: Arc<Loaded>,
    /// The plugin's own name for it.
    own: String,
    name: &'static str,
    description: &'static str,
    parameters: Value,
    ask: bool,
    read_only: bool,
    open_world: bool,
    destructive: bool,
    wait: Duration,
}

impl CodeTool {
    fn of(plugin: &Arc<Loaded>) -> Vec<Arc<dyn Tool>> {
        let code = &plugin.code;
        code.contributes
            .tools
            .iter()
            .map(|spec| {
                // Unsaid, a Python tool may reach anything, as an MCP
                // server's may; a JavaScript one only what its permission
                // lets it.
                let open_world = match code.runtime {
                    Runtime::Python => spec.open_world.unwrap_or(true),
                    Runtime::JavaScript => code.network || spec.open_world.unwrap_or(false),
                };
                let wait = spec
                    .timeout
                    .map_or(TOOL_WAIT, Duration::from_secs)
                    .min(LONGEST_TOOL_WAIT);
                Arc::new(CodeTool {
                    plugin: plugin.clone(),
                    own: spec.name.clone(),
                    name: crate::mcp::keep(tool_name(&plugin.name, &spec.name)),
                    description: crate::mcp::keep(spec.description.clone()),
                    parameters: spec.parameters.clone(),
                    // As a local MCP server's tool: only one that looks,
                    // and touches nothing past this computer, goes unasked.
                    ask: !(spec.read_only && !spec.destructive && !open_world),
                    read_only: spec.read_only,
                    open_world,
                    destructive: spec.destructive,
                    wait,
                }) as Arc<dyn Tool>
            })
            .collect()
    }
}

impl Tool for CodeTool {
    fn name(&self) -> &'static str {
        self.name
    }
    fn description(&self) -> &'static str {
        self.description
    }
    fn parameters(&self) -> Value {
        self.parameters.clone()
    }
    fn run(&self, args: &Value, ctx: &ToolContext) -> Result<String, String> {
        let message = json!({
            "type": "tool",
            "name": self.own,
            "args": args,
            "ctx": {"conversation": ctx.conversation, "workspace": ctx.workspace},
        });
        let said = match self.plugin.ask(message, self.wait, Some(&ctx.cancel)) {
            Waited::Said(said) => said,
            Waited::Failed(why) => return Err(why),
            Waited::Stopped => return Err(format!("{} was stopped while it ran.", self.own)),
            Waited::Late => {
                return Err(self.plugin.said(&format!(
                    "did not finish {} within {} s.",
                    self.own,
                    self.wait.as_secs()
                )));
            }
        };
        match said["error"].as_str() {
            Some(why) => Err(crate::untrusted::quoted(
                &format!("The {} plugin's {} failed:", self.plugin.name, self.own),
                why,
            )),
            None => Ok(text_of(&said["ok"])),
        }
    }
    fn needs_approval(&self) -> bool {
        self.ask
    }
    fn mutates(&self) -> bool {
        !self.read_only
    }
    fn destructive(&self) -> bool {
        self.destructive
    }
    fn open_world(&self) -> bool {
        self.open_world
    }
    fn untrusted(&self) -> bool {
        true
    }
    fn fences_its_errors(&self) -> bool {
        true
    }
    fn hides_key_shapes(&self) -> bool {
        true
    }
    fn preview(&self, args: &Value, _: &ToolContext) -> String {
        let args = serde_json::to_string_pretty(args).unwrap_or_default();
        format!(
            "{} from the {} plugin ({})\n{args}",
            self.own,
            self.plugin.name,
            self.plugin.code.runtime.name()
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::untrusted::holds_fence;

    /// A registry read from a data folder of its own, holding one plugin,
    /// allowed; its plugins stop and its folder goes when it is dropped.
    struct Built {
        data: PathBuf,
        registry: Registry,
    }

    impl std::ops::Deref for Built {
        type Target = Registry;
        fn deref(&self) -> &Registry {
            &self.registry
        }
    }

    impl Drop for Built {
        fn drop(&mut self) {
            for plugin in &self.registry.plugins {
                plugin.retire();
            }
            let _ = std::fs::remove_dir_all(&self.data);
        }
    }

    impl Built {
        fn tool(&self, name: &str) -> &Arc<dyn Tool> {
            (self.tools.iter())
                .find(|tool| tool.name() == name)
                .unwrap_or_else(|| panic!("no tool {name}"))
        }
    }

    /// The plugin `name`, whose plugin.json is `manifest` with its name, and
    /// `plugin.js` as main unless it says, and whose main file is `script`.
    fn plugin(name: &str, mut manifest: Value, script: &str) -> Built {
        let data =
            std::env::temp_dir().join(format!("letronna-code-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&data);
        let folder = data.join(super::super::PLUGINS_DIR).join(name);
        std::fs::create_dir_all(&folder).unwrap();
        manifest["name"] = name.into();
        if manifest.get("main").is_none() {
            manifest["main"] = "plugin.js".into();
        }
        let main = manifest["main"].as_str().unwrap().to_owned();
        std::fs::write(folder.join(super::super::MANIFEST), manifest.to_string()).unwrap();
        std::fs::write(folder.join(main), script).unwrap();
        super::super::allow_in(&data, name).unwrap();
        let registry = build(&data, None);
        assert_eq!(registry.plugins.len(), 1, "{name} did not load");
        Built { data, registry }
    }

    /// Waits for `done`, which another thread makes true, failing after a
    /// few seconds.
    fn wait_for(what: &str, done: impl Fn() -> bool) {
        let until = Instant::now() + Duration::from_secs(5);
        while !done() {
            assert!(Instant::now() < until, "waited in vain for {what}");
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    fn shown(plugin: &str) -> ui::Surfaces {
        ui::snapshot()
            .into_iter()
            .find(|(name, _)| name == plugin)
            .map(|(_, shown)| shown)
            .unwrap_or_default()
    }

    #[test]
    fn a_tool_answers_with_what_its_handler_gives_back() {
        let built = plugin(
            "words",
            json!({"contributes": {"tools": [
                {"name": "count", "description": "Counts words.", "readOnly": true},
                {"name": "later", "description": "Answers later."},
                {"name": "data", "description": "Gives data."},
                {"name": "broken", "description": "Throws."},
                {"name": "missing", "description": "Has no handler."},
            ]}}),
            r#"
            const { split } = require("./lib/split");
            letronna.tool("count", ({ text }) => String(split(text).length));
            letronna.tool("later", () => new Promise((done) => setTimeout(() => done("done"), 20)));
            letronna.tool("data", () => ({ n: 1 }));
            letronna.tool("broken", () => { throw new Error("no luck"); });
            "#,
        );
        let lib = built.data.join("plugins/words/lib");
        std::fs::create_dir_all(&lib).unwrap();
        std::fs::write(
            lib.join("split.js"),
            "exports.split = (text) => text.split(/\\s+/);",
        )
        .unwrap();
        let ctx = ToolContext::default();
        let count = built.tool("plugin__words__count");
        assert!(
            !count.needs_approval(),
            "it only looks, at nothing past here"
        );
        assert_eq!(count.run(&json!({"text": "a b c"}), &ctx).unwrap(), "3");
        let later = built.tool("plugin__words__later");
        assert!(later.needs_approval());
        assert_eq!(later.run(&json!({}), &ctx).unwrap(), "done");
        let data = built.tool("plugin__words__data").run(&json!({}), &ctx);
        assert_eq!(data.unwrap(), "{\n  \"n\": 1\n}");
        let error = built
            .tool("plugin__words__broken")
            .run(&json!({}), &ctx)
            .unwrap_err();
        assert!(error.contains("no luck") && holds_fence(&error), "{error}");
        let error = built
            .tool("plugin__words__missing")
            .run(&json!({}), &ctx)
            .unwrap_err();
        assert!(error.contains("no handler"), "{error}");
    }

    #[test]
    fn code_that_spins_or_grows_is_stopped_and_the_plugin_goes_on() {
        let built = plugin(
            "runaway",
            json!({"contributes": {"tools": [
                {"name": "spin", "description": "Spins."},
                {"name": "grow", "description": "Grows."},
                {"name": "fine", "description": "Answers."},
            ]}}),
            r#"
            letronna.tool("spin", () => { for (;;) {} });
            letronna.tool("grow", () => {
                const kept = [];
                for (;;) kept.push(new Array(100000).fill(1));
            });
            letronna.tool("fine", () => "still here");
            "#,
        );
        let ctx = ToolContext::default();
        let error = built
            .tool("plugin__runaway__spin")
            .run(&json!({}), &ctx)
            .unwrap_err();
        assert!(error.contains("ran past its time"), "{error}");
        let error = built
            .tool("plugin__runaway__grow")
            .run(&json!({}), &ctx)
            .unwrap_err();
        assert!(error.contains("memory"), "{error}");
        let fine = built.tool("plugin__runaway__fine").run(&json!({}), &ctx);
        assert_eq!(fine.unwrap(), "still here");
    }

    #[test]
    fn timers_fire_in_order_can_be_cleared_and_repeat_no_faster_than_allowed() {
        let built = plugin(
            "clock",
            json!({"contributes": {"tools": [
                {"name": "order", "description": "Orders."},
                {"name": "ticks", "description": "Counts ticks."},
            ]}}),
            r#"
            letronna.tool("order", () => new Promise((done) => {
                const seen = [];
                setTimeout(() => seen.push(3), 30);
                setTimeout(() => seen.push(1), 5);
                clearTimeout(setTimeout(() => seen.push("never"), 10));
                setTimeout(() => seen.push(2), 15);
                setTimeout(() => done(seen.join(",")), 60);
            }));
            letronna.tool("ticks", () => new Promise((done) => {
                let ticks = 0;
                const every = setInterval(() => { ticks += 1; }, 1);
                setTimeout(() => { clearInterval(every); done(String(ticks)); }, 350);
            }));
            "#,
        );
        let ctx = ToolContext::default();
        let order = built.tool("plugin__clock__order").run(&json!({}), &ctx);
        assert_eq!(order.unwrap(), "1,2,3");
        let ticks: u32 = built
            .tool("plugin__clock__ticks")
            .run(&json!({}), &ctx)
            .unwrap()
            .parse()
            .unwrap();
        assert!((2..=4).contains(&ticks), "{ticks} ticks in 350 ms");
    }

    #[test]
    fn fetch_needs_the_network_permission_and_never_reaches_this_machine() {
        let script =
            r#"letronna.tool("get", ({ url }) => letronna.fetch(url).then((r) => r.text()));"#;
        let tools = json!({"contributes": {"tools": [{"name": "get", "description": "Fetches."}]}});
        let ctx = ToolContext::default();
        let shut = plugin("shut", tools.clone(), script);
        let error = (shut.tool("plugin__shut__get"))
            .run(&json!({"url": "https://example.com"}), &ctx)
            .unwrap_err();
        assert!(error.contains("network permission"), "{error}");
        let mut asked = tools;
        asked["permissions"] = json!(["network"]);
        let open = plugin("open", asked, script);
        for url in [
            "https://127.0.0.1:9/",
            "https://localhost/",
            "https://192.168.1.1/",
            "http://example.com/",
        ] {
            let error = (open.tool("plugin__open__get"))
                .run(&json!({"url": url}), &ctx)
                .unwrap_err();
            assert!(
                error.contains("local network") || error.contains("https only"),
                "{url}: {error}"
            );
        }
    }

    #[test]
    fn storage_outlives_a_restart_and_is_capped() {
        let built = plugin(
            "keeper",
            json!({"contributes": {"tools": [
                {"name": "keep", "description": "Keeps."},
                {"name": "recall", "description": "Recalls."},
                {"name": "hoard", "description": "Hoards."},
            ]}}),
            r#"
            letronna.tool("keep", ({ value }) => { letronna.storage.set("kept", value); return "kept"; });
            letronna.tool("recall", () => String(letronna.storage.get("kept")));
            letronna.tool("hoard", () => {
                try { letronna.storage.set("big", "x".repeat(2 * 1024 * 1024)); return "kept"; }
                catch (error) { return String(error); }
            });
            "#,
        );
        let ctx = ToolContext::default();
        let keep = built.tool("plugin__keeper__keep");
        assert_eq!(keep.run(&json!({"value": "seven"}), &ctx).unwrap(), "kept");
        built.plugins[0].stop();
        let recall = built.tool("plugin__keeper__recall").run(&json!({}), &ctx);
        assert_eq!(recall.unwrap(), "seven");
        let hoard = built
            .tool("plugin__keeper__hoard")
            .run(&json!({}), &ctx)
            .unwrap();
        assert!(hoard.contains("at most 1 MB"), "{hoard}");
    }

    #[test]
    fn a_plugin_can_block_a_call_or_note_on_it_and_reads_its_words_only_when_allowed() {
        let script = r#"
            letronna.on("tool.before", (call) =>
                call.tool === "run_command" ? { block: "no commands today" } : `saw ${JSON.stringify(call)}`);
            letronna.on("tool.after", (call) => `after ${call.tool}: ${call.output ?? "unseen"}`);
        "#;
        let manifest = json!({"contributes": {"events": ["tool.before", "tool.after"]}});
        let ctx = ToolContext::default();
        let blind = plugin("blind", manifest.clone(), script);
        assert_eq!(blind.wants(), TOOL_BEFORE | TOOL_AFTER);
        let error = before_in(&blind, "run_command", &json!({"command": "ls"}), &ctx).unwrap_err();
        assert!(
            error.starts_with("The blind plugin blocked this call:"),
            "{error}"
        );
        assert!(
            error.contains("no commands today") && holds_fence(&error),
            "{error}"
        );
        let note = before_in(&blind, "read_file", &json!({"path": "secret.txt"}), &ctx).unwrap();
        assert!(note.contains("read_file") && holds_fence(&note), "{note}");
        assert!(
            !note.contains("secret.txt"),
            "it may not read the conversation: {note}"
        );
        let said = || json!({"args": {}, "output": "the text"});
        let after = ran_in(
            &blind,
            TOOL_AFTER,
            "tool.after",
            "read_file",
            Duration::ZERO,
            said,
            &ctx,
        );
        assert!(after.contains("unseen"), "{after}");

        let mut told = manifest;
        told["permissions"] = json!(["conversation"]);
        let reading = plugin("reading", told, script);
        let note = before_in(&reading, "read_file", &json!({"path": "secret.txt"}), &ctx).unwrap();
        assert!(note.contains("secret.txt"), "{note}");
        let said = || json!({"args": {}, "output": "the text"});
        let after = ran_in(
            &reading,
            TOOL_AFTER,
            "tool.after",
            "read_file",
            Duration::ZERO,
            said,
            &ctx,
        );
        assert!(after.contains("the text"), "{after}");
    }

    #[test]
    fn a_plugin_that_does_not_answer_in_time_lets_the_call_go_on() {
        let slow = plugin(
            "slow",
            json!({"contributes": {"events": ["tool.before"]}}),
            r#"letronna.on("tool.before", () => new Promise(() => {}));"#,
        );
        // Started, so the wait is a running plugin's.
        slow.plugins[0].ensure().unwrap();
        let started = Instant::now();
        let note = before_in(&slow, "read_file", &json!({}), &ToolContext::default());
        assert_eq!(note.unwrap(), "");
        let waited = started.elapsed();
        assert!(waited >= HOOK_WAIT && waited < COLD_HOOK_WAIT, "{waited:?}");
    }

    #[test]
    fn events_reach_the_plugins_that_hear_them_and_what_they_show_is_kept() {
        let built = plugin(
            "ticker",
            json!({"contributes": {
                "events": ["turn.end"], "status": true, "panel": {"title": "Ticker"},
            }}),
            r#"
            let turns = 0;
            letronna.on("turn.end", (turn) => {
                turns += 1;
                letronna.status(`${turns} turns, ${turn.answer ?? "unread"}`);
            });
            letronna.on("panel.open", () => letronna.ui.panel({
                type: "column", children: [{ type: "button", id: "again", label: "Again" }],
            }));
            letronna.ui.onAction("again", () => letronna.status("pressed"));
            "#,
        );
        assert_eq!(built.wants(), TURN_END);
        let bare = json!({"conversation": 1, "stopped": false});
        emit(
            &built,
            TURN_END,
            "turn.end",
            bare,
            || json!({"answer": "secret"}),
        );
        wait_for("the status", || shown("ticker").status == "1 turns, unread");
        let ticker = built.find("ticker").unwrap();
        ticker.post(json!({"type": "ui.open", "surface": "panel"}));
        wait_for("the panel", || shown("ticker").panel.is_some());
        ticker.post(json!({"type": "ui", "surface": "panel", "id": "again", "value": null}));
        wait_for("the press", || shown("ticker").status == "pressed");
        ticker.stop();
        assert_eq!(
            shown("ticker"),
            ui::Surfaces::default(),
            "a stopped plugin shows nothing"
        );
    }

    #[test]
    fn an_action_answers_with_text_or_words_for_the_box_and_its_settings_are_kept() {
        let built = plugin(
            "greeter",
            json!({"contributes": {
                "actions": [{"name": "greet", "description": "Says hello."}, {"name": "draft"}],
                "settings": [
                    {"key": "name", "type": "text", "default": "you"},
                    {"key": "loud", "type": "switch"},
                    {"key": "tone", "type": "choice", "choices": ["warm", "dry"]},
                ],
            }}),
            r#"
            letronna.action("greet", (args) => {
                const hi = `hello ${args || letronna.settings.get("name")}`;
                return letronna.settings.get("loud") ? hi.toUpperCase() : hi;
            });
            letronna.action("draft", () => ({ compose: "Write the release notes." }));
            "#,
        );
        let cancel = Cancel::default();
        let run = |name: &str, args: &str| run_action_in(&built, name, args, 1, None, &cancel);
        assert_eq!(run("greet", ""), Ok(Said::Text("hello you".into())));
        assert_eq!(run("greet", "Ada"), Ok(Said::Text("hello Ada".into())));
        assert_eq!(
            run("draft", ""),
            Ok(Said::Compose("Write the release notes.".into()))
        );
        assert!(run("nothing", "").is_err());
        let greeter = built.find("greeter").unwrap();
        assert_eq!(greeter.settings()["tone"], "warm");
        assert!(greeter.set("loud", json!("yes")).is_err());
        assert!(greeter.set("tone", json!("sharp")).is_err());
        greeter.set("loud", json!(true)).unwrap();
        assert_eq!(greeter.settings()["loud"], true);
        // The running code hears the change.
        wait_for("the setting", || {
            run("greet", "") == Ok(Said::Text("HELLO YOU".into()))
        });
    }

    #[test]
    fn only_a_plugin_switched_on_and_allowed_loads_and_nothing_starts_until_used() {
        let built = plugin(
            "quiet",
            json!({"contributes": {"events": ["tool.before"]}}),
            "",
        );
        assert!(!built.plugins[0].running());
        let data = built.data.clone();
        super::super::set_enabled_in(&data, "quiet", false).unwrap();
        assert!(build(&data, None).plugins.is_empty());
        super::super::set_enabled_in(&data, "quiet", true).unwrap();
        assert_eq!(build(&data, None).plugins.len(), 1);
        // Asking to read the conversation is a new question.
        let folder = data.join("plugins/quiet");
        let manifest = json!({
            "name": "quiet", "main": "plugin.js", "permissions": ["conversation"],
            "contributes": {"events": ["tool.before"]},
        });
        std::fs::write(folder.join("plugin.json"), manifest.to_string()).unwrap();
        assert!(build(&data, None).plugins.is_empty());
    }

    #[test]
    fn a_plugin_can_send_the_model_back_when_the_work_is_not_done() {
        let built = plugin(
            "checker",
            json!({"permissions": ["conversation"], "contributes": {"events": ["turn.stop"]}}),
            r#"
            letronna.on("turn.stop", (turn) => {
                if (turn.again) return;
                if (!turn.answer.includes("tests pass")) return "Run the tests: you did not say they pass.";
            });
            "#,
        );
        let ctx = ToolContext::default();
        let back = turn_stop_in(&built, &ctx, "I changed the parser.", false).unwrap();
        assert!(
            back.starts_with("The checker plugin says the work is not done:"),
            "{back}"
        );
        assert!(
            back.contains("Run the tests") && holds_fence(&back),
            "{back}"
        );
        assert_eq!(
            turn_stop_in(&built, &ctx, "Done, and the tests pass.", false),
            None
        );
        // It may let a turn sent back once end, as this one does.
        assert_eq!(
            turn_stop_in(&built, &ctx, "I changed it again.", true),
            None
        );
    }

    #[test]
    fn what_plugins_add_to_a_prompt_is_a_fenced_notice_and_reads_the_prompt_only_when_allowed() {
        let script = r#"
            letronna.on("prompt.submit", (sent) =>
                sent.text ? `The user asked about: ${sent.text}` : "The release branch is frozen.");
        "#;
        let manifest = json!({"contributes": {"events": ["prompt.submit"]}});
        let blind = plugin("hint", manifest.clone(), script);
        let cancel = Cancel::default();
        let added = prompt_context_in(&blind, 4, None, "fix the parser", &cancel).unwrap();
        assert!(added.notice && added.role == "user");
        assert!(
            added.content.starts_with(PROMPT_CONTEXT),
            "{}",
            added.content
        );
        assert!(
            added.content.contains("release branch is frozen"),
            "{}",
            added.content
        );
        assert!(holds_fence(&added.content));
        let mut told = manifest;
        told["permissions"] = json!(["conversation"]);
        let reading = plugin("reader", told, script);
        let added = prompt_context_in(&reading, 4, None, "fix the parser", &cancel).unwrap();
        assert!(
            added.content.contains("asked about: fix the parser"),
            "{}",
            added.content
        );
        let quiet = plugin(
            "quiet-prompt",
            json!({"contributes": {"events": ["prompt.submit"]}}),
            r#"letronna.on("prompt.submit", () => undefined);"#,
        );
        assert!(prompt_context_in(&quiet, 4, None, "hi", &cancel).is_none());
    }

    #[test]
    fn a_plugin_hears_calls_that_failed_and_how_long_they_took() {
        let built = plugin(
            "repeats",
            json!({"permissions": ["conversation"], "contributes": {"events": ["tool.failed"]}}),
            r#"
            const seen = new Map();
            letronna.on("tool.failed", (call) => {
                const key = `${call.tool} ${call.error}`;
                const times = (seen.get(key) ?? 0) + 1;
                seen.set(key, times);
                if (times > 1) return `This failed the same way ${times} times (last took ${call.ms} ms).`;
            });
            "#,
        );
        let ctx = ToolContext::default();
        let fail = || {
            let said = || json!({"args": {"command": "make"}, "error": "no rule to make target"});
            let took = Duration::from_millis(42);
            ran_in(
                &built,
                TOOL_FAILED,
                "tool.failed",
                "run_command",
                took,
                said,
                &ctx,
            )
        };
        assert_eq!(fail(), "");
        let note = fail();
        assert!(
            note.contains("same way 2 times (last took 42 ms)") && holds_fence(&note),
            "{note}"
        );
    }

    #[test]
    fn a_turns_tally_adds_up_its_replies_and_calls() {
        let mut tally = Tally::new();
        tally.replied(&json!({"input_tokens": 1000, "output_tokens": 200, "cached_tokens": 800, "cost": 0.01}));
        tally.replied(&json!({"input_tokens": 1500, "output_tokens": 100, "cost": 0.02}));
        tally.calls = 3;
        tally.failed = 1;
        let told = tally.told();
        assert_eq!(told["requests"], 2);
        assert_eq!(told["input_tokens"], 2500);
        assert_eq!(told["output_tokens"], 300);
        assert_eq!(told["cached_tokens"], 800);
        assert!((told["cost"].as_f64().unwrap() - 0.03).abs() < 1e-9);
        assert_eq!(
            (told["tool_calls"].clone(), told["tool_failures"].clone()),
            (json!(3), json!(1))
        );
        assert_eq!(
            Tally::new().told()["cost"],
            Value::Null,
            "no bill said is no cost known"
        );
    }

    /// Whether a Python to run plugins on is here; without one the Python
    /// tests check nothing, and say so.
    fn python_here() -> bool {
        let here = python::here();
        if !here {
            eprintln!("No Python 3.9 or later here, so this test checks nothing.");
        }
        here
    }

    #[test]
    fn a_python_plugin_answers_and_what_it_prints_goes_to_its_log() {
        if !python_here() {
            return;
        }
        let built = plugin(
            "pywords",
            json!({"main": "plugin.py", "contributes": {
                "tools": [
                    {"name": "count", "description": "Counts words."},
                    {"name": "where", "description": "Says where."},
                    {"name": "broken", "description": "Raises."},
                ],
                "actions": [{"name": "shout"}],
                "events": ["tool.before"],
                "settings": [{"key": "loud", "type": "switch", "default": true}],
            }}),
            r#"
import letronna
print("hello from print")

@letronna.tool("count")
def count(args, ctx):
    print("counting", args)
    return str(len(args["text"].split()))

@letronna.tool("where")
async def where(args, ctx):
    return {"conversation": ctx.conversation, "plugin": letronna.plugin}

@letronna.tool("broken")
def broken(args):
    raise ValueError("no luck")

@letronna.action("shout")
def shout(text):
    return text.upper() if letronna.settings.get("loud") else text

@letronna.on("tool.before")
def guard(call):
    if call["tool"] == "run_command":
        return {"block": "not from Python either"}
"#,
        );
        let ctx = ToolContext {
            conversation: 7,
            ..Default::default()
        };
        let count = built.tool("plugin__pywords__count");
        assert_eq!(count.run(&json!({"text": "a b c d"}), &ctx).unwrap(), "4");
        let said = built
            .tool("plugin__pywords__where")
            .run(&json!({}), &ctx)
            .unwrap();
        let said: Value = serde_json::from_str(&said).unwrap();
        assert_eq!(said, json!({"conversation": 7, "plugin": "pywords"}));
        let error = built
            .tool("plugin__pywords__broken")
            .run(&json!({}), &ctx)
            .unwrap_err();
        assert!(error.contains("ValueError: no luck"), "{error}");
        let cancel = Cancel::default();
        let shouted = run_action_in(&built, "shout", "hi", 1, None, &cancel);
        assert_eq!(shouted, Ok(Said::Text("HI".into())));
        let blocked = before_in(&built, "run_command", &json!({}), &ctx).unwrap_err();
        assert!(blocked.contains("not from Python either"), "{blocked}");
        let log = built.data.join("plugin-data/pywords/log.txt");
        wait_for("the log", || {
            std::fs::read_to_string(&log).is_ok_and(|log| log.contains("hello from print"))
        });
        assert!(
            !built.data.join("plugins/pywords/__pycache__").exists(),
            "nothing is written into the folder the user allowed"
        );
    }

    #[test]
    fn a_program_a_python_plugin_starts_reads_nothing_of_its_messages() {
        if !python_here() {
            return;
        }
        // Given the messages' pipe, the program would wait on it, and on
        // Windows a git started like this never even began.
        let built = plugin(
            "pyquiet",
            json!({"main": "plugin.py", "contributes": {
                "tools": [{"name": "child", "description": "Starts a program."}],
            }}),
            r#"
import subprocess, sys
import letronna

@letronna.tool("child")
def child(args):
    done = subprocess.run(
        [sys.executable, "-c", "import sys; print(repr(sys.stdin.read()))"],
        capture_output=True, text=True, timeout=5,
    )
    return done.stdout.strip()
"#,
        );
        let ctx = ToolContext::default();
        let child = built.tool("plugin__pyquiet__child");
        assert_eq!(child.run(&json!({}), &ctx).unwrap(), "''");
    }

    #[test]
    fn a_python_plugin_that_dies_starts_again_after_a_wait_and_stops_when_switched_off() {
        if !python_here() {
            return;
        }
        let built = plugin(
            "pydies",
            json!({"main": "plugin.py", "contributes": {"tools": [
                {"name": "pid", "description": "Says its process."},
                {"name": "die", "description": "Dies."},
            ]}}),
            r#"
import os, letronna

@letronna.tool("pid")
def pid(args):
    return str(os.getpid())

@letronna.tool("die")
def die(args):
    os._exit(3)
"#,
        );
        let ctx = ToolContext::default();
        let pid = built.tool("plugin__pydies__pid");
        let first: u32 = pid.run(&json!({}), &ctx).unwrap().parse().unwrap();
        let error = built
            .tool("plugin__pydies__die")
            .run(&json!({}), &ctx)
            .unwrap_err();
        assert!(error.contains("process ended"), "{error}");
        let error = pid.run(&json!({}), &ctx).unwrap_err();
        assert!(error.contains("starts again in"), "{error}");
        std::thread::sleep(Duration::from_millis(1_100));
        let second: u32 = pid.run(&json!({}), &ctx).unwrap().parse().unwrap();
        assert_ne!(first, second);
        built.plugins[0].retire();
        #[cfg(unix)]
        wait_for("its process to end", || {
            // SAFETY: signal 0 only asks whether the process is there.
            unsafe { libc::kill(second as libc::pid_t, 0) != 0 }
        });
    }

    /// The example plugin `name` from the repository, put in a data folder
    /// of its own and allowed.
    fn example(name: &str) -> Built {
        let from = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../examples/plugins")
            .join(name);
        let data =
            std::env::temp_dir().join(format!("letronna-example-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&data);
        let folder = data.join(super::super::PLUGINS_DIR).join(name);
        std::fs::create_dir_all(&folder).unwrap();
        for entry in std::fs::read_dir(&from).unwrap().flatten() {
            std::fs::copy(entry.path(), folder.join(entry.file_name())).unwrap();
        }
        super::super::allow_in(&data, name).unwrap();
        let registry = build(&data, None);
        assert_eq!(registry.plugins.len(), 1, "the {name} example did not load");
        Built { data, registry }
    }

    #[test]
    fn the_guard_example_blocks_what_is_hard_to_take_back_and_lets_the_rest_through() {
        let guard = example("guard");
        let ctx = ToolContext::default();
        let run =
            |command: &str| before_in(&guard, "run_command", &json!({"command": command}), &ctx);
        let error = run("git push --force origin main").unwrap_err();
        assert!(error.contains("force push"), "{error}");
        assert!(run("git reset --hard HEAD~3").is_err());
        assert!(run("rm -rf ~").is_err());
        assert_eq!(run("git push origin main").unwrap(), "");
        assert_eq!(run("rm -rf target").unwrap(), "");
        guard
            .find("guard")
            .unwrap()
            .set("force_push", json!(false))
            .unwrap();
        wait_for("the switch", || run("git push --force origin main").is_ok());
        let cancel = Cancel::default();
        let said = run_action_in(&guard, "guard", "", 1, None, &cancel).unwrap();
        assert!(
            matches!(&said, Said::Text(text) if text.contains("blocked 3")),
            "{said:?}"
        );
    }

    #[test]
    fn the_git_status_example_says_what_changed_in_the_sessions_project() {
        if !python_here() {
            return;
        }
        let status = example("git-status");
        let project = status.data.join("project");
        std::fs::create_dir_all(&project).unwrap();
        let git = |args: &[&str]| {
            let done = std::process::Command::new("git")
                .args([
                    "-c",
                    "user.name=t",
                    "-c",
                    "user.email=t@t",
                    "-c",
                    "commit.gpgsign=false",
                ])
                .args(args)
                .current_dir(&project)
                .output()
                .unwrap();
            assert!(
                done.status.success(),
                "{}",
                String::from_utf8_lossy(&done.stderr)
            );
        };
        git(&["init", "--quiet", "-b", "main"]);
        std::fs::write(project.join("notes.txt"), "one").unwrap();
        let summary = status.tool("plugin__git-status__summary");
        assert!(!summary.needs_approval(), "it only looks, here");
        let ctx = ToolContext {
            workspace: Some(project.clone()),
            ..Default::default()
        };
        let said = summary.run(&json!({}), &ctx).unwrap();
        assert!(
            said.contains("On main; 1 changed") && said.contains("notes.txt"),
            "{said}"
        );
        let bare = json!({"conversation": 1, "workspace": project, "stopped": false});
        emit(&status, TURN_END, "turn.end", bare, || json!({}));
        wait_for("the status", || {
            shown("git-status").status == "main · 1 changed"
        });
        wait_for("the panel", || shown("git-status").panel.is_some());
        // Switched on, it tells the model with each message.
        let cancel = Cancel::default();
        assert!(prompt_context_in(&status, 1, Some(&project), "hi", &cancel).is_none());
        status
            .find("git-status")
            .unwrap()
            .set("tell_model", json!(true))
            .unwrap();
        wait_for("the setting", || {
            prompt_context_in(&status, 1, Some(&project), "hi", &cancel)
                .is_some_and(|added| added.content.contains("on branch main, with 1 changed"))
        });
    }

    #[test]
    fn the_session_meter_example_shows_what_a_turn_used_and_speaks_up_on_repeated_failures() {
        let meter = example("session-meter");
        let reply =
            json!({"conversation": 9, "model": "m", "input_tokens": 1200, "output_tokens": 300});
        emit(&meter, MODEL_REPLY, "model.reply", reply, || json!({}));
        wait_for("the live status", || {
            shown("session-meter").status == "1.5k tokens so far"
        });
        let mut ended = json!({"conversation": 9, "stopped": false});
        let mut used = Tally::new();
        used.replied(&json!({"input_tokens": 1200, "output_tokens": 300, "cost": 0.004}));
        merge(&mut ended, &used.told());
        emit(
            &meter,
            TURN_END,
            "turn.end",
            ended,
            || json!({"answer": "done"}),
        );
        wait_for("the turn's status", || {
            shown("session-meter")
                .status
                .starts_with("1.5k tokens · $0.004 · ")
        });
        let ctx = ToolContext {
            conversation: 9,
            ..Default::default()
        };
        let fail = || {
            let said = || json!({"args": {}, "error": "permission denied"});
            ran_in(
                &meter,
                TOOL_FAILED,
                "tool.failed",
                "run_command",
                Duration::ZERO,
                said,
                &ctx,
            )
        };
        assert_eq!(fail(), "");
        let note = fail();
        assert!(note.contains("failed 2 times this turn"), "{note}");
        let cancel = Cancel::default();
        let said = run_action_in(&meter, "meter", "", 9, None, &cancel).unwrap();
        assert!(
            matches!(&said, Said::Text(text) if text.starts_with("1 turns")),
            "{said:?}"
        );
    }

    #[test]
    fn the_test_gate_example_sends_the_model_back_until_the_tests_pass() {
        if !python_here() {
            return;
        }
        let gate = example("test-gate");
        let project = gate.data.join("project");
        std::fs::create_dir_all(&project).unwrap();
        let git = |args: &[&str]| {
            let done = std::process::Command::new("git")
                .args([
                    "-c",
                    "user.name=t",
                    "-c",
                    "user.email=t@t",
                    "-c",
                    "commit.gpgsign=false",
                ])
                .args(args)
                .current_dir(&project)
                .output()
                .unwrap();
            assert!(
                done.status.success(),
                "{}",
                String::from_utf8_lossy(&done.stderr)
            );
        };
        git(&["init", "--quiet"]);
        std::fs::write(project.join("ok"), "no").unwrap();
        let ctx = ToolContext {
            workspace: Some(project.clone()),
            ..Default::default()
        };
        let plugin = gate.find("test-gate").unwrap();
        // Off until a command is set.
        assert_eq!(turn_stop_in(&gate, &ctx, "Done.", false), None);
        // Windows has python; a python3 there may be the store's stand-in.
        let python = if cfg!(windows) { "python" } else { "python3" };
        let test = format!(
            r#"{python} -c "import sys; ok = open('ok').read(); print('1 failed' if ok != 'yes' else 'all pass'); sys.exit(ok != 'yes')""#
        );
        plugin.set("command", json!(test)).unwrap();
        wait_for("the setting", || {
            turn_stop_in(&gate, &ctx, "Done.", false).is_some_and(|back| back.contains("1 failed"))
        });
        std::fs::write(project.join("ok"), "yes").unwrap();
        assert_eq!(turn_stop_in(&gate, &ctx, "Fixed.", true), None);
        wait_for("the status", || shown("test-gate").status == "tests pass");
    }

    #[test]
    fn a_read_only_tool_goes_unasked_only_when_it_reaches_nothing_past_this_computer() {
        let looks = json!([{"name": "look", "description": "Looks.", "readOnly": true}]);
        let local = plugin("local", json!({"contributes": {"tools": looks}}), "");
        assert!(!local.tool("plugin__local__look").needs_approval());
        let online = plugin(
            "online",
            json!({"permissions": ["network"], "contributes": {"tools": looks}}),
            "",
        );
        assert!(online.tool("plugin__online__look").needs_approval());
        let snake = plugin(
            "snake",
            json!({"main": "plugin.py", "contributes": {"tools": looks}}),
            "",
        );
        assert!(
            snake.tool("plugin__snake__look").needs_approval(),
            "unsaid, a Python tool may reach anything"
        );
        let said = json!([{"name": "look", "description": "Looks.", "readOnly": true, "openWorld": false}]);
        let tame = plugin(
            "tame",
            json!({"main": "plugin.py", "contributes": {"tools": said}}),
            "",
        );
        assert!(!tame.tool("plugin__tame__look").needs_approval());
    }
}
