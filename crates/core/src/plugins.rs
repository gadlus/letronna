//! Plugins: a git repository whose `plugin.json` brings skills, slash
//! commands, output styles, MCP servers and code in JavaScript or Python in
//! one go (see `code`).
//!
//! A plugin is cloned with `git clone --depth 1` into a staging folder and
//! scanned there before anything in the data folder changes: its files pass
//! the path, name and size rules a skill install does, it holds no link, its
//! manifest names no absolute path, and no program sits outside the folders
//! it declares. Only then is it moved to `<data>/plugins/<name>`, where the
//! registries find it: its skills after the user's own and before a
//! project's, so a plugin cannot shadow a skill the user relies on, and its
//! MCP servers as `<plugin>:<server>`, a name `mcp.json` cannot already give
//! a server of the user's. Commands and styles are listed by folder for the
//! registries that read those files.
//!
//! A server is somebody else's program, so none of a plugin's starts until
//! the user has allowed that plugin's servers once. The consent is kept as
//! the version and a hash of the server entries it was given for, so an
//! update that changes either asks again, and a plugin with no servers never
//! asks. A plugin's code waits for the same consent, which then also covers
//! what the code may reach and brings, and, for a plugin installed from
//! somewhere, the code itself. What is kept per plugin -- where it came from, whether it is off,
//! what was allowed -- lives in `plugins.json`, along with the marketplace
//! index links. A marketplace is only a JSON list of plugins someone
//! publishes; nothing here is hosted.
//!
//! An update clones afresh and scans that before it takes the old copy's
//! place, rather than pulling into the folder in use, so a bad update
//! changes nothing. A removed plugin, and the copy an update replaces, are
//! moved into `plugins-archive` whole, as skills are.

use crate::chat as backend;
use crate::skills::{self, Source};
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::{Map, Value};
use std::collections::BTreeMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

pub mod code;

/// The file that makes a folder a plugin.
const MANIFEST: &str = "plugin.json";
/// The plugins, one folder each, in the data folder.
const PLUGINS_DIR: &str = "plugins";
/// What is kept per plugin, and the marketplace links, in the data folder.
const STATE_FILE: &str = "plugins.json";
/// Where a removed or replaced plugin goes, whole.
const ARCHIVE: &str = "plugins-archive";
/// Where a clone is scanned before it goes anywhere.
const INCOMING: &str = "plugins-incoming";
/// A manifest lists a few folders and servers; a bigger one is not a
/// manifest.
const MANIFEST_BYTES: u64 = 256_000;
/// The longest plugin or server name: it becomes a folder and part of every
/// tool name the server gives, which providers cap at 64.
const NAME_CHARS: usize = 40;
/// How deep a plugin's folders are walked by the scan.
const DEPTH: usize = 16;
/// Entries read from one marketplace index.
const MARKET_ENTRIES: usize = 500;
/// The starts of the files a computer runs as a program: ELF, Windows' MZ,
/// Mach-O thin in both byte orders and 32 or 64 bits, Mach-O fat, and
/// WebAssembly.
const PROGRAMS: &[&[u8]] = &[
    b"\x7fELF",
    b"MZ",
    b"\xfe\xed\xfa\xce",
    b"\xfe\xed\xfa\xcf",
    b"\xce\xfa\xed\xfe",
    b"\xcf\xfa\xed\xfe",
    b"\xca\xfe\xba\xbe",
    b"\0asm",
];

/// A plugin as the lists show it: what it is, and what it brings.
#[derive(Clone, Debug, PartialEq)]
pub struct Plugin {
    /// Its folder's name, which is the name its manifest gave when it came.
    pub name: String,
    pub version: String,
    pub description: String,
    /// The git URL or folder it was cloned from; empty for one put in the
    /// folder by hand.
    pub source: String,
    pub folder: PathBuf,
    /// Switched on: what it brings is offered.
    pub enabled: bool,
    /// Its MCP servers may start and its code run: the user allowed them as
    /// they are in this version, or it has none.
    pub allowed: bool,
    pub skills: Vec<String>,
    pub commands: Vec<String>,
    pub styles: Vec<String>,
    /// Its MCP servers, each with what starts it: the command line or URL.
    pub servers: Vec<(String, String)>,
    /// The providers it declares, each with the URL its requests go to.
    pub providers: Vec<(String, String)>,
    /// The code it runs, and what that brings.
    pub code: Option<code::Code>,
    /// What it brings that is left out, each with why.
    pub clashes: Vec<String>,
    /// Why its manifest does not read now, when it does not; then it brings
    /// nothing.
    pub broken: Option<String>,
}

/// One MCP server a plugin brings, as `mcp` starts it.
#[derive(Clone, Debug, PartialEq)]
pub struct Server {
    /// `<plugin>:<server>`.
    pub name: String,
    pub plugin: String,
    /// The plugin's folder, which the server starts in unless its entry
    /// gives a `cwd` inside it.
    pub folder: PathBuf,
    /// Its entry, as `mcp.json` would hold it.
    pub config: Value,
}

/// One plugin a marketplace index lists.
#[derive(Clone, Debug, PartialEq)]
pub struct Listing {
    pub name: String,
    pub description: String,
    /// What `add` is given to install it: a git URL or owner/repo.
    pub repo: String,
}

/// One marketplace index and what it lists, or why it could not be read.
#[derive(Clone, Debug)]
pub struct Market {
    pub url: String,
    pub listings: Result<Vec<Listing>, String>,
}

/// `plugin.json`: its name, what it is, and the folders and servers it
/// brings.
#[derive(Deserialize, Default)]
struct Manifest {
    #[serde(default)]
    name: String,
    #[serde(default)]
    version: String,
    #[serde(default)]
    description: String,
    #[serde(default, deserialize_with = "one_or_many")]
    skills: Vec<String>,
    #[serde(default, deserialize_with = "one_or_many")]
    commands: Vec<String>,
    #[serde(default, alias = "outputStyles", deserialize_with = "one_or_many")]
    styles: Vec<String>,
    #[serde(default, rename = "mcpServers")]
    servers: Map<String, Value>,
    /// OpenAI-compatible APIs it offers as providers: data, never a program.
    #[serde(default)]
    providers: Vec<crate::providers::declared::Entry>,
    /// The code it runs, as written.
    #[serde(flatten)]
    code: code::Spec,
    /// That code, as `read_manifest` checked it.
    #[serde(skip)]
    checked: Option<code::Code>,
}

/// A folder given as one path or a list of them.
fn one_or_many<'de, D: Deserializer<'de>>(from: D) -> Result<Vec<String>, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Given {
        One(String),
        Many(Vec<String>),
    }
    Ok(match Given::deserialize(from)? {
        Given::One(one) => vec![one],
        Given::Many(many) => many,
    })
}

/// `plugins.json`. Keys it does not know, which a newer Letronna may write,
/// are kept.
#[derive(Deserialize, Serialize, Default)]
struct Saved {
    #[serde(default)]
    plugins: BTreeMap<String, Record>,
    #[serde(default)]
    markets: Vec<String>,
    #[serde(flatten)]
    rest: Map<String, Value>,
}

/// What is kept for one plugin.
#[derive(Deserialize, Serialize, Default, Clone)]
struct Record {
    #[serde(default)]
    source: String,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    off: bool,
    /// The servers the user allowed, as `consent_key` put them then.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    allowed: String,
    #[serde(flatten)]
    rest: Map<String, Value>,
}

/// The plugins folder.
pub fn dir() -> PathBuf {
    backend::data_dir().join(PLUGINS_DIR)
}

/// Every plugin installed, by name, with what each brings and what of it is
/// left out.
pub fn list() -> Result<Vec<Plugin>, String> {
    list_in(&backend::data_dir())
}

fn list_in(data: &Path) -> Result<Vec<Plugin>, String> {
    let saved = read_saved(data)?;
    // A plugin's skill is left out when the user has one by that name, or
    // an earlier plugin does: the order the skills are gathered in.
    let mut taken: Vec<String> = skills::read_root(&data.join(skills::SKILLS_DIR), Source::User)
        .into_iter()
        .map(|skill| skill.name)
        .collect();
    let mut plugins = Vec::new();
    for folder in plugin_folders(data) {
        let name = folder_name(&folder);
        let record = saved.plugins.get(&name).cloned().unwrap_or_default();
        plugins.push(described(&folder, name, &record, &mut taken));
    }
    Ok(plugins)
}

/// One plugin: `name`'s entry of `list`.
fn plugin_in(data: &Path, name: &str) -> Result<Plugin, String> {
    list_in(data)?
        .into_iter()
        .find(|plugin| plugin.name == name)
        .ok_or_else(|| missing(name))
}

/// The plugin in `folder` as the list shows it. Its skills whose names are
/// in `taken` go into its clashes; the rest are added to `taken` when it is
/// on.
fn described(folder: &Path, name: String, record: &Record, taken: &mut Vec<String>) -> Plugin {
    let mut plugin = Plugin {
        name,
        version: String::new(),
        description: String::new(),
        source: record.source.clone(),
        folder: folder.to_path_buf(),
        enabled: !record.off,
        allowed: false,
        skills: Vec::new(),
        commands: Vec::new(),
        styles: Vec::new(),
        servers: Vec::new(),
        providers: Vec::new(),
        code: None,
        clashes: Vec::new(),
        broken: None,
    };
    let manifest = match read_manifest(folder) {
        Ok(manifest) => manifest,
        Err(why) => {
            plugin.broken = Some(why);
            return plugin;
        }
    };
    plugin.version = skills::printable(&manifest.version);
    plugin.description = skills::printable(&manifest.description);
    let consent = consent_key(folder, &manifest, record);
    plugin.allowed = consent.is_empty() || record.allowed == consent;
    plugin.code = manifest.checked.clone();
    for root in skill_folders(folder, &manifest) {
        for skill in skills::read_root(&root, Source::Plugin) {
            if taken.iter().any(|name| same(name, &skill.name)) {
                plugin.clashes.push(format!(
                    "The skill \"{}\" is left out: a skill of that name is already yours or \
                     another plugin's.",
                    skill.name
                ));
                continue;
            }
            if plugin.enabled {
                taken.push(skill.name.clone());
            }
            plugin.skills.push(skill.name);
        }
    }
    for root in declared(folder, &manifest.commands, "commands") {
        markdown_names(&root, "", 2, &mut plugin.commands);
    }
    for root in declared(folder, &manifest.styles, "styles") {
        markdown_names(&root, "", 0, &mut plugin.styles);
    }
    plugin.servers = manifest
        .servers
        .iter()
        .map(|(server, config)| (format!("{}:{server}", plugin.name), target(config)))
        .collect();
    for entry in &manifest.providers {
        match crate::providers::declared::refused(entry) {
            Some(why) => plugin.clashes.push(why),
            None => plugin
                .providers
                .push((entry.name.trim().to_owned(), entry.base.trim().to_owned())),
        }
    }
    plugin
}

/// Names compare without case, as skills are asked for.
fn same(a: &str, b: &str) -> bool {
    a.to_lowercase() == b.to_lowercase()
}

/// What starts a server, as its entry has it, for the user to judge.
fn target(config: &Value) -> String {
    if let Some(url) = config["url"].as_str().filter(|url| !url.trim().is_empty()) {
        // A header can carry `${VAR}` from the user's environment, a key
        // say, to wherever the URL points, so the user sees it before
        // allowing.
        let headers: Vec<String> = config["headers"]
            .as_object()
            .into_iter()
            .flatten()
            .map(|(name, value)| format!("{name}: {}", value.as_str().unwrap_or_default()))
            .collect();
        return match headers.is_empty() {
            true => skills::printable(url),
            false => skills::printable(&format!("{url} with {}", headers.join(", "))),
        };
    }
    let words = std::iter::once(&config["command"])
        .chain(config["args"].as_array().into_iter().flatten())
        .filter_map(Value::as_str);
    skills::printable(&words.collect::<Vec<_>>().join(" "))
}

/// The Markdown files in `folder`, as commands and styles are named: the
/// file's name less `.md`, after `prefix`, looking `depth` subfolders down
/// with their names joined by `:`.
fn markdown_names(folder: &Path, prefix: &str, depth: usize, names: &mut Vec<String>) {
    let Ok(entries) = std::fs::read_dir(folder) else {
        return;
    };
    let mut entries: Vec<_> = entries.flatten().collect();
    entries.sort_by_key(|entry| entry.file_name());
    for entry in entries {
        let name = entry.file_name().to_string_lossy().into_owned();
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        if name.starts_with('.') {
            continue;
        }
        if kind.is_dir() && depth > 0 {
            markdown_names(&entry.path(), &format!("{prefix}{name}:"), depth - 1, names);
        } else if let Some(stem) = name.strip_suffix(".md").filter(|_| kind.is_file()) {
            names.push(format!("{prefix}{stem}"));
        }
    }
}

/// The plugin folders, by name, hidden ones left out.
fn plugin_folders(data: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(data.join(PLUGINS_DIR)) else {
        return Vec::new();
    };
    let mut folders: Vec<PathBuf> = entries
        .flatten()
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
        .map(|entry| entry.path())
        .filter(|path| !folder_name(path).starts_with('.'))
        .collect();
    folders.sort();
    folders
}

fn folder_name(folder: &Path) -> String {
    folder
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// The plugins switched on whose manifests read, each with it.
fn active(data: &Path) -> Vec<(String, PathBuf, Manifest, Record)> {
    // One that will not parse is reported where the list is shown.
    let saved = read_saved(data).unwrap_or_default();
    plugin_folders(data)
        .into_iter()
        .filter_map(|folder| {
            let name = folder_name(&folder);
            let record = saved.plugins.get(&name).cloned().unwrap_or_default();
            if record.off {
                return None;
            }
            let manifest = read_manifest(&folder).ok()?;
            Some((name, folder, manifest, record))
        })
        .collect()
}

/// The plugins switched on and allowed whose code reads, each with its
/// folder and that code.
pub(crate) fn coded_in(data: &Path) -> Vec<(String, PathBuf, code::Code)> {
    active(data)
        .into_iter()
        .filter(|(_, folder, manifest, record)| {
            record.allowed == consent_key(folder, manifest, record)
        })
        .filter_map(|(name, folder, manifest, _)| Some((name, folder, manifest.checked?)))
        .collect()
}

/// The folders of skill folders the plugins switched on bring, for the
/// skills to be gathered from after the user's own.
pub(crate) fn skill_roots_in(data: &Path) -> Vec<PathBuf> {
    active(data)
        .iter()
        .flat_map(|(_, folder, manifest, _)| skill_folders(folder, manifest))
        .collect()
}

/// The folders of command files the plugins switched on bring.
pub fn command_folders() -> Vec<PathBuf> {
    command_folders_in(&backend::data_dir())
}

/// `command_folders` for the data folder `data`.
pub(crate) fn command_folders_in(data: &Path) -> Vec<PathBuf> {
    active(data)
        .iter()
        .flat_map(|(_, folder, manifest, _)| declared(folder, &manifest.commands, "commands"))
        .collect()
}

/// The folders of output style files the plugins switched on bring.
pub fn style_folders() -> Vec<PathBuf> {
    active(&backend::data_dir())
        .iter()
        .flat_map(|(_, folder, manifest, _)| declared(folder, &manifest.styles, "styles"))
        .collect()
}

/// The MCP servers of the plugins switched on whose servers the user has
/// allowed as they are now.
pub fn servers() -> Vec<Server> {
    servers_in(&backend::data_dir())
}

fn servers_in(data: &Path) -> Vec<Server> {
    let mut servers = Vec::new();
    for (plugin, folder, manifest, record) in active(data) {
        if record.allowed != consent_key(&folder, &manifest, &record) {
            continue;
        }
        for (name, config) in &manifest.servers {
            let mut config = config.clone();
            if let Some(entry) = config.as_object_mut() {
                // Started in its own folder, so `node server.js` finds its file.
                entry.entry("cwd").or_insert_with(|| Value::from("."));
                // Whoever wrote a plugin may not be whoever runs it, so it
                // cannot let its own tools past the question, as a
                // project's permissions.json cannot.
                entry.remove("autoApprove");
            }
            servers.push(Server {
                name: format!("{plugin}:{name}"),
                plugin: plugin.clone(),
                folder: folder.clone(),
                config,
            });
        }
    }
    servers
}

/// The providers of the plugins switched on and allowed, in plugin order.
pub(crate) fn providers() -> Vec<crate::providers::declared::Entry> {
    active(&backend::data_dir())
        .into_iter()
        .filter(|(_, folder, manifest, record)| {
            record.allowed == consent_key(folder, manifest, record)
        })
        .flat_map(|(_, _, manifest, _)| manifest.providers)
        .collect()
}

/// What the user agrees to by allowing a plugin's servers, providers and
/// code: its version and a hash of their entries, and of the code itself
/// when the plugin came from somewhere. Empty for a plugin with none.
fn consent_key(folder: &Path, manifest: &Manifest, record: &Record) -> String {
    if manifest.servers.is_empty() && manifest.providers.is_empty() && manifest.checked.is_none() {
        return String::new();
    }
    // Servers alone hash as they always did, so their consent still holds,
    // and so do servers and providers without code. With code, the keys are
    // put in order first: whether serde_json keeps a map's order depends on
    // what else a binary is built with, and the window and the terminal
    // must agree on what the user allowed.
    let entries = match (manifest.providers.is_empty(), manifest.checked.is_none()) {
        (true, true) => serde_json::to_vec(&manifest.servers),
        (false, true) => serde_json::to_vec(&(&manifest.servers, &manifest.providers)),
        (_, false) => {
            serde_json::to_value((&manifest.servers, &manifest.providers, &manifest.code))
                .and_then(|entries| serde_json::to_vec(&in_order(entries)))
        }
    }
    .unwrap_or_default();
    let mut hash = ring::digest::Context::new(&ring::digest::SHA256);
    hash.update(&entries);
    // The user's own edits to a plugin they put in the folder themselves
    // are theirs; one installed from somewhere asks again for new code.
    if manifest.checked.is_some() && !record.source.is_empty() {
        code::digest(folder, &mut hash);
    }
    let hash = hash.finish();
    let hex: String = hash.as_ref()[..8]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    format!("{}#{hex}", manifest.version)
}

/// `value` with every object's keys in order, however the map it was read
/// into keeps them.
fn in_order(value: Value) -> Value {
    match value {
        Value::Object(map) => {
            let mut entries: Vec<(String, Value)> = map.into_iter().collect();
            entries.sort_by(|a, b| a.0.cmp(&b.0));
            Value::Object(
                entries
                    .into_iter()
                    .map(|(key, value)| (key, in_order(value)))
                    .collect(),
            )
        }
        Value::Array(items) => Value::Array(items.into_iter().map(in_order).collect()),
        other => other,
    }
}

/// The skill folders a plugin declares: `skills`, or its `skills` folder
/// when it names none.
fn skill_folders(folder: &Path, manifest: &Manifest) -> Vec<PathBuf> {
    declared(folder, &manifest.skills, skills::SKILLS_DIR)
}

/// The folders `given` names inside `folder`, or the folder called `usual`
/// when none are named and it is there. The scan made sure each is inside.
fn declared(folder: &Path, given: &[String], usual: &str) -> Vec<PathBuf> {
    if given.is_empty() {
        let usual = folder.join(usual);
        return if usual.is_dir() {
            vec![usual]
        } else {
            Vec::new()
        };
    }
    given
        .iter()
        .filter_map(|path| inside(path).ok())
        .map(|path| folder.join(path))
        .collect()
}

/// Install the plugin at `source`: a git URL, a GitHub repository as
/// owner/repo, or a git repository on this computer. It starts switched on,
/// with its MCP servers waiting for `allow`. This waits on the network, so
/// call it off the UI thread.
pub fn add(source: &str) -> Result<Plugin, String> {
    add_in(&backend::data_dir(), source)
}

fn add_in(data: &Path, source: &str) -> Result<Plugin, String> {
    let url = git_url(source)?;
    let staging = staging_folder(data);
    let placed = fetch(&url, &staging).and_then(|manifest| {
        let target = data.join(PLUGINS_DIR).join(&manifest.name);
        if target.exists() {
            return Err(format!(
                "A plugin called {} is installed already. Update it instead, or remove it first.",
                manifest.name
            ));
        }
        std::fs::create_dir_all(data.join(PLUGINS_DIR))
            .and_then(|()| std::fs::rename(&staging, &target))
            .map_err(|e| format!("Could not move {} into place: {e}", manifest.name))?;
        // A record left by an earlier plugin of this name, consent and all,
        // is not this one's.
        change_saved(data, |saved| {
            saved.plugins.insert(
                manifest.name.clone(),
                Record {
                    source: url.clone(),
                    ..Record::default()
                },
            );
            Ok(())
        })?;
        Ok(manifest.name)
    });
    tidy(data, &staging);
    plugin_in(data, &placed?)
}

/// Clone `name` again from where it came and put that in its place once it
/// has been scanned, archiving the copy it replaces. None when nothing
/// changed; else where the earlier copy went. Its switch is kept, and so is
/// the consent unless its version or servers changed.
pub fn update(name: &str) -> Result<Option<PathBuf>, String> {
    let done = update_in(&backend::data_dir(), name, || {
        crate::mcp::stop_plugin(name);
        // A process with its folder open would keep the folder from moving
        // on Windows.
        code::stop(name);
    });
    // Stopped or not, its servers start from whichever copy is in place.
    let restarted = restart_servers(name);
    let done = done?;
    restarted?;
    Ok(done)
}

/// `update` in `data`, calling `before_move` just before the old copy moves.
fn update_in(
    data: &Path,
    name: &str,
    before_move: impl FnOnce(),
) -> Result<Option<PathBuf>, String> {
    let folder = installed(data, name)?;
    let saved = read_saved(data)?;
    let source = saved
        .plugins
        .get(name)
        .map(|record| record.source.clone())
        .filter(|source| !source.is_empty())
        .ok_or_else(|| {
            format!("{name} was not installed from anywhere, so it cannot be updated.")
        })?;
    let staging = staging_folder(data);
    let done = fetch(&source, &staging).and_then(|manifest| {
        if manifest.name != name {
            return Err(format!(
                "{source} now calls itself {}, not {name}; remove {name} and add it again.",
                manifest.name
            ));
        }
        if head(&staging).is_some() && head(&staging) == head(&folder) {
            return Ok(None);
        }
        before_move();
        let kept = archive(data, &folder)?;
        if let Err(e) = std::fs::rename(&staging, &folder) {
            let back = std::fs::rename(&kept, &folder)
                .err()
                .map(|e| format!(" It could not be put back from {}: {e}", kept.display()))
                .unwrap_or_default();
            return Err(format!(
                "Could not move the new {name} into place: {e}.{back}"
            ));
        }
        Ok(Some(kept))
    });
    tidy(data, &staging);
    done
}

/// The commit a clone is at, if git can say.
fn head(folder: &Path) -> Option<String> {
    let out = git(&["rev-parse", "HEAD"], Some(folder)).ok()?;
    Some(out.trim().to_owned()).filter(|sha| !sha.is_empty())
}

/// Take a plugin out: its folder goes into `plugins-archive`, whole, and
/// what was kept for it is forgotten. Says where it went.
pub fn remove(name: &str) -> Result<PathBuf, String> {
    let kept = remove_in(&backend::data_dir(), name, || {
        crate::mcp::stop_plugin(name);
        code::stop(name);
    });
    // Started again from the folder left in place if the move failed.
    let restarted = restart_servers(name);
    let kept = kept?;
    restarted?;
    Ok(kept)
}

/// `remove` in `data`, calling `before_move` just before the folder moves.
fn remove_in(data: &Path, name: &str, before_move: impl FnOnce()) -> Result<PathBuf, String> {
    let folder = installed(data, name)?;
    before_move();
    let kept = archive(data, &folder)?;
    change_saved(data, |saved| {
        saved.plugins.remove(name);
        Ok(())
    })
    .map_err(|why| format!("Moved {name} to {}, but: {why}", kept.display()))?;
    Ok(kept)
}

/// Switch a plugin on or off; off, nothing it brings is offered and its
/// servers stop.
pub fn set_enabled(name: &str, on: bool) -> Result<(), String> {
    set_enabled_in(&backend::data_dir(), name, on)?;
    restart_servers(name)
}

fn set_enabled_in(data: &Path, name: &str, on: bool) -> Result<(), String> {
    installed(data, name)?;
    change_saved(data, |saved| {
        saved.plugins.entry(name.to_owned()).or_default().off = !on;
        Ok(())
    })
}

/// Allow a plugin's MCP servers to start, as they are in the version
/// installed now, and start them.
pub fn allow(name: &str) -> Result<(), String> {
    allow_in(&backend::data_dir(), name)?;
    restart_servers(name)
}

fn allow_in(data: &Path, name: &str) -> Result<(), String> {
    let folder = installed(data, name)?;
    let manifest = read_manifest(&folder)?;
    let record = read_saved(data)?
        .plugins
        .get(name)
        .cloned()
        .unwrap_or_default();
    let key = consent_key(&folder, &manifest, &record);
    if key.is_empty() {
        return Err(format!(
            "{name} brings no MCP servers, providers or code, so there is nothing to allow."
        ));
    }
    change_saved(data, |saved| {
        saved.plugins.entry(name.to_owned()).or_default().allowed = key;
        Ok(())
    })
}

/// Bring the MCP servers and the plugins' code in line after `name` changed.
fn restart_servers(name: &str) -> Result<(), String> {
    crate::providers::reload();
    code::reload();
    crate::mcp::reload()
        .map(drop)
        .map_err(|why| format!("{name} changed, but its MCP servers did not follow: {why}"))
}

/// The folder of the installed plugin `name`.
fn installed(data: &Path, name: &str) -> Result<PathBuf, String> {
    let folder = data.join(PLUGINS_DIR).join(name);
    // A name that is a path would reach outside the plugins folder.
    if !valid_name(name) || !folder.is_dir() {
        return Err(missing(name));
    }
    Ok(folder)
}

fn missing(name: &str) -> String {
    format!("No plugin called {name} is installed.")
}

/// A plugin or server name: letters, digits, `-` and `_`, starting with a
/// letter or digit, as a folder and a tool name take it everywhere.
fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.chars().count() <= NAME_CHARS
        && name.starts_with(|c: char| c.is_ascii_alphanumeric())
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'))
        && skills::plain(name)
}

/// The git URL `source` means: a URL as given, a repository on this
/// computer by its full path, or owner/repo on GitHub.
fn git_url(source: &str) -> Result<String, String> {
    let source = source.trim().trim_matches('"').trim();
    if source.is_empty() {
        return Err("Say where from: a git URL, or a GitHub repository as owner/repo.".into());
    }
    if source.starts_with("http://") {
        return Err("Plugins are cloned over https or ssh, not http.".into());
    }
    if ["https://", "ssh://", "git@", "file://"]
        .iter()
        .any(|scheme| source.starts_with(scheme))
    {
        return Ok(source.to_owned());
    }
    // `-` would read as an option to git.
    if source.starts_with('-') {
        return Err(format!("`{source}` is not a git URL."));
    }
    let path = Path::new(source);
    if path.exists() {
        return std::path::absolute(path)
            .map(|path| path.to_string_lossy().into_owned())
            .map_err(|e| format!("Could not read {source}: {e}"));
    }
    let parts: Vec<&str> = source.split('/').collect();
    let named = parts.len() == 2
        && parts.iter().all(|part| {
            !matches!(*part, "" | "." | "..")
                && part
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
        });
    if !named {
        return Err(format!(
            "Nothing at `{source}`. Give a git URL, or a GitHub repository as owner/repo."
        ));
    }
    let repo = parts[1].trim_end_matches(".git");
    Ok(format!("https://github.com/{}/{repo}.git", parts[0]))
}

/// A folder of its own for one clone, so two at once cannot mix.
fn staging_folder(data: &Path) -> PathBuf {
    static COUNT: AtomicU64 = AtomicU64::new(0);
    let n = COUNT.fetch_add(1, Ordering::Relaxed);
    data.join(INCOMING)
        .join(format!("{}-{}-{n}", std::process::id(), backend::now()))
}

/// Take away what a clone left in staging. Whatever is there is a copy, so
/// a failure to tidy it changes nothing that matters.
fn tidy(data: &Path, staging: &Path) {
    let _ = std::fs::remove_dir_all(staging);
    let _ = std::fs::remove_dir(data.join(INCOMING));
}

/// Clone `url` into `staging`, then read and scan what came; its manifest
/// once nothing in it is refused.
fn fetch(url: &str, staging: &Path) -> Result<Manifest, String> {
    if let Some(parent) = staging.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("Could not make {}: {e}", parent.display()))?;
    }
    let into = staging.to_string_lossy();
    git(
        &["clone", "--depth", "1", "--quiet", "--", url, &into],
        None,
    )
    .map_err(|why| format!("Could not clone {url}: {why}"))?;
    let manifest = read_manifest(staging)?;
    scan(staging, &manifest)?;
    Ok(manifest)
}

/// Run git with `args`, in `folder` if given, and what it printed; never
/// asking for a password, which nobody would see.
fn git(args: &[&str], folder: Option<&Path>) -> Result<String, String> {
    let mut command = std::process::Command::new("git");
    command
        .args(args)
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdin(std::process::Stdio::null());
    if let Some(folder) = folder {
        command.current_dir(folder);
    }
    no_window(&mut command);
    let out = command
        .output()
        .map_err(|e| format!("git could not be started ({e}); plugins need git installed."))?;
    if !out.status.success() {
        let said = String::from_utf8_lossy(&out.stderr);
        let last = said.lines().rev().find(|line| !line.trim().is_empty());
        return Err(skills::printable(last.unwrap_or("git failed.")));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Keeps a console window from flashing up for git on Windows, where
/// Letronna owns none.
fn no_window(command: &mut std::process::Command) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    #[cfg(not(windows))]
    let _ = command;
}

/// The manifest in `folder`, checked: a valid name, folders inside it, and
/// servers whose entries read with no absolute path in them.
fn read_manifest(folder: &Path) -> Result<Manifest, String> {
    let file = folder.join(MANIFEST);
    if !file.is_file() {
        return Err(format!(
            "There is no {MANIFEST} in {}.",
            folder_name(folder)
        ));
    }
    let mut bytes = Vec::new();
    std::fs::File::open(&file)
        .and_then(|opened| opened.take(MANIFEST_BYTES + 1).read_to_end(&mut bytes))
        .map_err(|e| format!("Could not read {}: {e}", file.display()))?;
    if bytes.len() as u64 > MANIFEST_BYTES {
        return Err(format!("{MANIFEST} is over {MANIFEST_BYTES} bytes."));
    }
    let mut manifest: Manifest =
        serde_json::from_slice(&bytes).map_err(|e| format!("{MANIFEST} will not read: {e}"))?;
    if !valid_name(&manifest.name) {
        return Err(format!(
            "{MANIFEST} names the plugin \"{}\"; a name is up to {NAME_CHARS} letters, digits, \
             - and _.",
            skills::printable(&manifest.name)
        ));
    }
    for path in manifest
        .skills
        .iter()
        .chain(&manifest.commands)
        .chain(&manifest.styles)
    {
        let path = inside(path)?;
        if !folder.join(&path).is_dir() {
            return Err(format!(
                "{MANIFEST} names the folder {path}, which is not there."
            ));
        }
    }
    for (name, config) in &manifest.servers {
        if !valid_name(name) {
            return Err(format!(
                "{MANIFEST} names a server \"{}\"; a name is up to {NAME_CHARS} letters, \
                 digits, - and _.",
                skills::printable(name)
            ));
        }
        crate::mcp::readable(config).map_err(|why| format!("The server {name}: {why}"))?;
        let words = [&config["command"], &config["cwd"]]
            .into_iter()
            .chain(config["args"].as_array().into_iter().flatten())
            .filter_map(Value::as_str);
        for word in words {
            if absolute(word) {
                return Err(format!(
                    "The server {name} names {word}, an absolute path: a plugin's servers run \
                     from its own folder."
                ));
            }
        }
        if let Some(cwd) = config["cwd"].as_str() {
            inside(cwd).map_err(|why| format!("The server {name}'s cwd: {why}"))?;
        }
    }
    manifest.checked = code::checked(&manifest.name, folder, &manifest.code)?;
    Ok(manifest)
}

/// `path` as a folder inside the plugin, `/`-separated: relative, with no
/// `..` and every part a name any system can write.
fn inside(path: &str) -> Result<String, String> {
    let trimmed = path.trim().trim_start_matches("./").trim_end_matches('/');
    if trimmed.is_empty() || trimmed == "." {
        return Ok(".".into());
    }
    if absolute(trimmed) {
        return Err(format!(
            "{path} is an absolute path; a plugin names its own folders."
        ));
    }
    if !trimmed.split('/').all(skills::plain) {
        return Err(format!("{path} is not a folder inside the plugin."));
    }
    Ok(trimmed.to_owned())
}

/// Whether `path` names a place from the top of a disk or a home folder, on
/// any system.
fn absolute(path: &str) -> bool {
    let bytes = path.as_bytes();
    let drive = bytes.first().is_some_and(u8::is_ascii_alphabetic)
        && bytes.get(1) == Some(&b':')
        && matches!(bytes.get(2), Some(b'\\' | b'/'));
    drive || path.starts_with(['/', '\\', '~']) || Path::new(path).is_absolute()
}

/// Refuse a plugin whose files would not install as a skill's would -- a
/// name that cannot be written everywhere, too many or too big -- or that
/// holds a link, or a program outside the folders its manifest declares.
fn scan(folder: &Path, manifest: &Manifest) -> Result<(), String> {
    let mut files = Vec::new();
    walk(folder, folder, 0, &mut files)?;
    for (relative, ..) in &files {
        if skills::safe(relative).is_none() {
            return Err(skills::unwritable(relative));
        }
    }
    skills::within_limits(
        files
            .iter()
            .map(|(relative, size, _)| (relative.as_str(), *size)),
    )?;
    let code: u64 = files
        .iter()
        .filter(|(relative, ..)| code::is_code(relative))
        .map(|(_, size, _)| size)
        .sum();
    if code > code::CODE_BYTES {
        return Err(format!(
            "The plugin holds {code} bytes of code, more than the {} a plugin may, so it was \
             not installed.",
            code::CODE_BYTES
        ));
    }
    let homes: Vec<String> = skill_folders(folder, manifest)
        .into_iter()
        .chain(declared(folder, &manifest.commands, "commands"))
        .chain(declared(folder, &manifest.styles, "styles"))
        .filter_map(|home| home.strip_prefix(folder).ok().map(slashed))
        .collect();
    for (relative, _, path) in &files {
        let at_home = homes
            .iter()
            .any(|home| home.is_empty() || relative.starts_with(&format!("{home}/")));
        if !at_home && program(path) {
            return Err(format!(
                "{relative} is a program, outside the folders {MANIFEST} declares, so the plugin \
                 was not installed."
            ));
        }
    }
    Ok(())
}

/// Every file under `folder`, `/`-separated from `top`, with its size; the
/// clone's own `.git` left out. A link is refused, as it could point
/// anywhere, and so is a tree deeper than `DEPTH`.
fn walk(
    top: &Path,
    folder: &Path,
    depth: usize,
    found: &mut Vec<(String, u64, PathBuf)>,
) -> Result<(), String> {
    let entries = std::fs::read_dir(folder)
        .map_err(|e| format!("Could not read {}: {e}", folder.display()))?;
    for entry in entries.flatten() {
        let path = entry.path();
        let relative = path.strip_prefix(top).map(slashed).unwrap_or_default();
        if depth == 0 && relative == ".git" {
            continue;
        }
        let kind = entry
            .file_type()
            .map_err(|e| format!("Could not read {relative}: {e}"))?;
        if kind.is_symlink() {
            return Err(format!(
                "{relative} is a link, which could point outside the plugin, so it was not \
                 installed."
            ));
        }
        if kind.is_dir() {
            if depth >= DEPTH {
                return Err(format!(
                    "{relative} is nested deeper than a plugin's folders go."
                ));
            }
            walk(top, &path, depth + 1, found)?;
        } else {
            let size = entry.metadata().map_or(0, |meta| meta.len());
            found.push((relative, size, path));
        }
    }
    Ok(())
}

fn slashed(path: &Path) -> String {
    path.components()
        .map(|part| part.as_os_str().to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join("/")
}

/// Whether the file at `path` starts as a program does.
fn program(path: &Path) -> bool {
    let mut head = Vec::new();
    let read = std::fs::File::open(path).and_then(|file| file.take(4).read_to_end(&mut head));
    read.is_ok() && PROGRAMS.iter().any(|magic| head.starts_with(magic))
}

/// Move a plugin's folder into the archive, whole, and say where it went.
fn archive(data: &Path, folder: &Path) -> Result<PathBuf, String> {
    let archive = data.join(ARCHIVE);
    let mut target = archive.join(format!("{}-{}", folder_name(folder), backend::now()));
    let mut n = 2;
    while target.exists() {
        target = archive.join(format!("{}-{}-{n}", folder_name(folder), backend::now()));
        n += 1;
    }
    std::fs::create_dir_all(&archive)
        .and_then(|()| std::fs::rename(folder, &target))
        .map_err(|e| {
            format!(
                "Could not move {} into {}: {e}",
                folder.display(),
                archive.display()
            )
        })?;
    Ok(target)
}

/// `plugins.json` as it is now.
fn read_saved(data: &Path) -> Result<Saved, String> {
    crate::state::read::<Saved>(&data.join(STATE_FILE))
}

/// Change `plugins.json` under its lock, writing nothing when `change`
/// refuses.
fn change_saved(
    data: &Path,
    change: impl FnOnce(&mut Saved) -> Result<(), String>,
) -> Result<(), String> {
    let path = data.join(STATE_FILE);
    std::fs::create_dir_all(data).map_err(|e| format!("{}: {e}", data.display()))?;
    let mut outcome = Ok(());
    crate::state::update::<Saved>(&path, |value| {
        let read = match value {
            Value::Null => Ok(Saved::default()),
            _ => serde_json::from_value::<Saved>(value.clone()),
        };
        let mut saved = match read {
            Ok(saved) => saved,
            Err(e) => {
                outcome = Err(format!("{} will not read: {e}", path.display()));
                return;
            }
        };
        outcome = change(&mut saved);
        if outcome.is_err() {
            return;
        }
        match serde_json::to_value(&saved) {
            Ok(changed) => *value = changed,
            Err(e) => outcome = Err(format!("Could not write {}: {e}", path.display())),
        }
    })?;
    outcome
}

/// The marketplace index links, in the order they were added.
pub fn markets() -> Result<Vec<String>, String> {
    Ok(read_saved(&backend::data_dir())?.markets)
}

/// Add a marketplace index: an https link to a JSON list of plugins, or such
/// a file on this computer.
pub fn add_market(url: &str) -> Result<(), String> {
    add_market_in(&backend::data_dir(), url)
}

fn add_market_in(data: &Path, url: &str) -> Result<(), String> {
    let url = url.trim().to_owned();
    if !url.starts_with("https://") && !Path::new(&url).is_file() {
        return Err(format!(
            "{url} is neither an https link nor a file on this computer."
        ));
    }
    change_saved(data, |saved| {
        if saved.markets.contains(&url) {
            return Err(format!("{url} is listed already."));
        }
        saved.markets.push(url.clone());
        Ok(())
    })
}

/// Stop listing a marketplace index.
pub fn remove_market(url: &str) -> Result<(), String> {
    change_saved(&backend::data_dir(), |saved| {
        let before = saved.markets.len();
        saved.markets.retain(|kept| kept != url.trim());
        if saved.markets.len() == before {
            return Err(format!("{} is not one of your marketplaces.", url.trim()));
        }
        Ok(())
    })
}

/// Every marketplace index and what it lists, each read afresh. This waits
/// on the network, so call it off the UI thread.
pub fn market() -> Result<Vec<Market>, String> {
    market_in(&backend::data_dir())
}

fn market_in(data: &Path) -> Result<Vec<Market>, String> {
    Ok(read_saved(data)?
        .markets
        .into_iter()
        .map(|url| {
            let listings = index_bytes(&url).and_then(|bytes| parse_index(&bytes));
            Market { url, listings }
        })
        .collect())
}

/// An index's bytes, from the web or this computer, as big as a skill's
/// file may be.
fn index_bytes(url: &str) -> Result<Vec<u8>, String> {
    if !url.starts_with("https://") {
        let mut bytes = Vec::new();
        std::fs::File::open(url)
            .and_then(|file| file.take(skills::FILE_BYTES).read_to_end(&mut bytes))
            .map_err(|e| format!("Could not read {url}: {e}"))?;
        return Ok(bytes);
    }
    let link = reqwest::Url::parse(url).map_err(|_| format!("{url} is not a link."))?;
    skills::get(&skills::client()?, link, url).map(|(bytes, _)| bytes)
}

/// The plugins an index lists: a JSON list of `{name, description, repo}`,
/// or an object holding one as `plugins`. An entry with no name or repo is
/// passed over.
fn parse_index(bytes: &[u8]) -> Result<Vec<Listing>, String> {
    let value: Value =
        serde_json::from_slice(bytes).map_err(|e| format!("The index will not read: {e}"))?;
    let entries = value
        .as_array()
        .or_else(|| value["plugins"].as_array())
        .ok_or("The index is not a list of plugins.")?;
    let text =
        |entry: &Value, key: &str| skills::printable(entry[key].as_str().unwrap_or_default());
    Ok(entries
        .iter()
        .take(MARKET_ENTRIES)
        .map(|entry| Listing {
            name: text(entry, "name"),
            description: text(entry, "description"),
            repo: text(entry, "repo"),
        })
        .filter(|listing| !listing.name.is_empty() && !listing.repo.is_empty())
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        // The process's id too, so two checkouts testing at once keep apart.
        let dir =
            std::env::temp_dir().join(format!("letronna-plugins-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write(path: &Path, bytes: &[u8]) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, bytes).unwrap();
    }

    /// Commit everything in `repo`, making it a repository first if need be.
    fn commit(repo: &Path) {
        let run = |args: &[&str]| {
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
                .current_dir(repo)
                .output()
                .unwrap();
            assert!(
                done.status.success(),
                "{}",
                String::from_utf8_lossy(&done.stderr)
            );
        };
        if !repo.join(".git").exists() {
            run(&["init", "--quiet"]);
        }
        run(&["add", "--all"]);
        run(&["commit", "--quiet", "--allow-empty", "-m", "change"]);
    }

    /// A repository holding a plugin called `name` with one skill, one
    /// command, one style and, when `server` is set, one MCP server.
    fn plugin_repo(root: &Path, name: &str, skill: &str, server: bool) -> PathBuf {
        let repo = root.join(format!("{name}-source"));
        let mut manifest = serde_json::json!({
            "name": name, "version": "1.0.0", "description": "A test plugin.",
        });
        if server {
            manifest["mcpServers"] =
                serde_json::json!({"files": {"command": "node", "args": ["server.js"]}});
        }
        write(&repo.join(MANIFEST), manifest.to_string().as_bytes());
        write(
            &repo.join(format!("skills/{skill}/SKILL.md")),
            format!("---\nname: {skill}\ndescription: Does a thing.\n---\nDo it.\n").as_bytes(),
        );
        write(&repo.join("commands/ship.md"), b"Ship it.");
        write(&repo.join("styles/terse.md"), b"Be terse.");
        commit(&repo);
        repo
    }

    fn source(repo: &Path) -> String {
        repo.to_string_lossy().into_owned()
    }

    #[test]
    fn a_plugins_skills_join_the_list_and_its_commands_and_styles_are_named() {
        let data = scratch("merge");
        let repo = plugin_repo(&data, "tools", "Tidy imports", false);
        let plugin = add_in(&data, &source(&repo)).unwrap();
        assert_eq!(plugin.name, "tools");
        assert_eq!(plugin.version, "1.0.0");
        assert_eq!(plugin.skills, ["Tidy imports"]);
        assert_eq!(plugin.commands, ["ship"]);
        assert_eq!(plugin.styles, ["terse"]);
        assert!(
            plugin.allowed,
            "a plugin with no servers has nothing to allow"
        );
        let listed = skills::list_in(&data, None).unwrap();
        let skill = skills::find(&listed, "Tidy imports").unwrap();
        assert_eq!(skill.source, Source::Plugin);
        assert!(
            skill
                .folder
                .starts_with(data.join(PLUGINS_DIR).join("tools"))
        );
    }

    #[test]
    fn a_plugins_provider_joins_the_registry_once_allowed_and_cannot_take_a_built_in_name() {
        let _held = crate::providers::auth::tests::vault("plugin-provider");
        let data = backend::data_dir();
        let manifest = serde_json::json!({
            "name": "acme", "version": "1.0.0",
            "providers": [
                {"name": "Acme", "base": "https://api.acme.test/v1", "tier": "credit",
                 "blurb": "Acme's models."},
                {"name": "openai", "base": "https://elsewhere.test/v1"},
                {"name": "Loose", "base": "http://example.test/v1"},
            ],
        });
        write(
            &data.join(PLUGINS_DIR).join("acme").join(MANIFEST),
            manifest.to_string().as_bytes(),
        );
        let plugin = plugin_in(&data, "acme").unwrap();
        assert_eq!(
            plugin.providers,
            [("Acme".to_owned(), "https://api.acme.test/v1".to_owned())]
        );
        assert!(
            plugin
                .clashes
                .iter()
                .any(|why| why.contains("built-in provider"))
        );
        assert!(plugin.clashes.iter().any(|why| why.contains("https://")));
        // It sends messages elsewhere, so it waits to be allowed.
        assert!(!plugin.allowed);
        crate::providers::reload();
        assert!(crate::providers::find("Acme").is_none());

        allow_in(&data, "acme").unwrap();
        crate::providers::reload();
        let acme = crate::providers::find("Acme").unwrap();
        assert_eq!(acme.info().tier, crate::providers::Tier::Credit);
        assert_eq!(acme.auth(), crate::providers::AuthKind::ApiKey);
        assert_eq!(
            crate::providers::find("OpenAI").unwrap().info().base,
            "https://api.openai.com/v1"
        );
        // Routed like any other once its key is saved under its name.
        let keys = std::collections::HashMap::from([("Acme".to_owned(), "k".to_owned())]);
        let ready = crate::gateway::ready(&keys, "");
        assert!(
            ready
                .iter()
                .any(|g| *g == ("Acme", "https://api.acme.test/v1".into(), "k".into()))
        );

        set_enabled_in(&data, "acme", false).unwrap();
        crate::providers::reload();
        assert!(crate::providers::find("Acme").is_none());
    }

    #[test]
    fn a_plugin_cannot_shadow_a_built_in_skill() {
        let data = scratch("clash");
        // Lays down the skills Letronna comes with.
        let before = skills::list_in(&data, None).unwrap();
        let built_in = skills::find(&before, "Run the tests")
            .unwrap()
            .folder
            .clone();
        let repo = plugin_repo(&data, "sneaky", "Run the tests", false);
        let plugin = add_in(&data, &source(&repo)).unwrap();
        assert!(plugin.skills.is_empty());
        assert_eq!(plugin.clashes.len(), 1);
        assert!(plugin.clashes[0].contains("Run the tests"));
        let after = skills::list_in(&data, None).unwrap();
        let kept = skills::find(&after, "Run the tests").unwrap();
        assert_eq!(kept.source, Source::User);
        assert_eq!(kept.folder, built_in);
    }

    #[test]
    fn the_scan_refuses_a_bad_plugin_and_installs_nothing() {
        let data = scratch("scan");
        let refused = |repo: &Path, why: &str| {
            let error = add_in(&data, &source(repo)).unwrap_err();
            assert!(error.contains(why), "{error}");
            assert!(plugin_folders(&data).is_empty());
            assert!(!data.join(INCOMING).exists());
        };

        let program = plugin_repo(&data, "program", "One", false);
        write(&program.join("bin/tool"), b"\x7fELF\x02\x01\x01");
        commit(&program);
        refused(&program, "is a program");

        let far = plugin_repo(&data, "far", "Two", true);
        let manifest = serde_json::json!({
            "name": "far", "mcpServers": {"files": {"command": "/usr/bin/node"}},
        });
        write(&far.join(MANIFEST), manifest.to_string().as_bytes());
        commit(&far);
        refused(&far, "absolute path");

        let outside = plugin_repo(&data, "outside", "Three", false);
        let manifest = serde_json::json!({"name": "outside", "skills": "../elsewhere"});
        write(&outside.join(MANIFEST), manifest.to_string().as_bytes());
        commit(&outside);
        refused(&outside, "not a folder inside");

        let big = plugin_repo(&data, "big", "Four", false);
        write(
            &big.join("blob.txt"),
            &vec![b'a'; skills::FILE_BYTES as usize + 1],
        );
        commit(&big);
        refused(&big, "bytes");

        let named = plugin_repo(&data, "named", "Five", false);
        write(&named.join(MANIFEST), br#"{"name": "../up"}"#);
        commit(&named);
        refused(&named, "names the plugin");

        // A program inside a declared folder is the plugin's to bring.
        let fine = plugin_repo(&data, "fine", "Six", false);
        write(&fine.join("skills/Six/run"), b"\x7fELF\x02\x01\x01");
        commit(&fine);
        assert!(add_in(&data, &source(&fine)).is_ok());
    }

    #[test]
    fn a_plugins_servers_start_only_once_allowed_and_ask_again_when_they_change() {
        let data = scratch("consent");
        let repo = plugin_repo(&data, "files", "Seven", true);
        let plugin = add_in(&data, &source(&repo)).unwrap();
        assert!(!plugin.allowed);
        assert_eq!(
            plugin.servers,
            [("files:files".to_owned(), "node server.js".to_owned())]
        );
        assert!(servers_in(&data).is_empty());

        allow_in(&data, "files").unwrap();
        let servers = servers_in(&data);
        assert_eq!(servers.len(), 1);
        assert_eq!(servers[0].name, "files:files");
        assert_eq!(servers[0].config["cwd"], ".");
        assert!(plugin_in(&data, "files").unwrap().allowed);

        // A new version with other servers is a new question.
        let manifest = serde_json::json!({
            "name": "files", "version": "2.0.0",
            "mcpServers": {"files": {"command": "node", "args": ["other.js"]}},
        });
        write(&repo.join(MANIFEST), manifest.to_string().as_bytes());
        commit(&repo);
        // Its servers are stopped while the old copy is still in place.
        let folder = data.join(PLUGINS_DIR).join("files");
        let mut stopped_in_place = None;
        let kept = update_in(&data, "files", || {
            stopped_in_place = Some(folder.join(MANIFEST).is_file());
        })
        .unwrap()
        .unwrap();
        assert_eq!(stopped_in_place, Some(true));
        assert!(kept.starts_with(data.join(ARCHIVE)));
        assert!(servers_in(&data).is_empty());
        assert!(!plugin_in(&data, "files").unwrap().allowed);
        let mut stopped = false;
        assert_eq!(update_in(&data, "files", || stopped = true).unwrap(), None);
        assert!(!stopped, "nothing changed, so nothing stops");
    }

    #[test]
    fn what_a_server_sends_is_shown_before_it_is_allowed() {
        let web = serde_json::json!({
            "url": "https://example.com/mcp",
            "headers": {"Authorization": "Bearer ${GITHUB_TOKEN}"},
        });
        assert_eq!(
            target(&web),
            "https://example.com/mcp with Authorization: Bearer ${GITHUB_TOKEN}"
        );
        let bare = serde_json::json!({"url": "https://example.com/mcp"});
        assert_eq!(target(&bare), "https://example.com/mcp");
    }

    #[test]
    fn a_plugins_servers_cannot_approve_their_own_tools() {
        let data = scratch("approve");
        let repo = plugin_repo(&data, "eager", "Nine", true);
        let manifest = serde_json::json!({
            "name": "eager", "version": "1.0.0",
            "mcpServers": {"files": {"command": "node", "autoApprove": ["delete_file"]}},
        });
        write(&repo.join(MANIFEST), manifest.to_string().as_bytes());
        commit(&repo);
        add_in(&data, &source(&repo)).unwrap();
        allow_in(&data, "eager").unwrap();
        let servers = servers_in(&data);
        assert_eq!(servers.len(), 1);
        assert!(servers[0].config.get("autoApprove").is_none());
    }

    #[test]
    fn a_plugin_switched_off_brings_nothing_and_removing_it_archives_it() {
        let data = scratch("switch");
        let repo = plugin_repo(&data, "kit", "Eight", true);
        add_in(&data, &source(&repo)).unwrap();
        allow_in(&data, "kit").unwrap();
        assert_eq!(skill_roots_in(&data).len(), 1);
        assert_eq!(servers_in(&data).len(), 1);

        set_enabled_in(&data, "kit", false).unwrap();
        assert!(!plugin_in(&data, "kit").unwrap().enabled);
        assert!(skill_roots_in(&data).is_empty());
        assert!(servers_in(&data).is_empty());
        assert!(skills::find(&skills::list_in(&data, None).unwrap(), "Eight").is_none());

        set_enabled_in(&data, "kit", true).unwrap();
        assert_eq!(
            servers_in(&data).len(),
            1,
            "the consent outlasts the switch"
        );

        let folder = data.join(PLUGINS_DIR).join("kit");
        let mut stopped_in_place = false;
        let kept = remove_in(&data, "kit", || stopped_in_place = folder.is_dir()).unwrap();
        assert!(stopped_in_place);
        assert!(kept.join(MANIFEST).is_file());
        assert!(list_in(&data).unwrap().is_empty());
        assert!(set_enabled_in(&data, "kit", true).is_err());
        assert!(set_enabled_in(&data, "../kit", true).is_err());
    }

    #[test]
    fn a_marketplace_index_lists_its_plugins_from_either_shape() {
        let data = scratch("market");
        let index = data.join("index.json");
        write(
            &index,
            br#"{"plugins": [
                {"name": "kit", "description": "Tools\nfor things", "repo": "someone/kit"},
                {"name": "no-repo"}
            ]}"#,
        );
        add_market_in(&data, &source(&index)).unwrap();
        assert!(add_market_in(&data, &source(&index)).is_err());
        assert!(add_market_in(&data, "http://example.com/index.json").is_err());
        let markets = market_in(&data).unwrap();
        let listings = markets[0].listings.as_ref().unwrap();
        assert_eq!(
            listings,
            &[Listing {
                name: "kit".into(),
                description: "Tools for things".into(),
                repo: "someone/kit".into(),
            }]
        );
        assert_eq!(
            parse_index(br#"[{"name": "a", "repo": "b/c"}]"#)
                .unwrap()
                .len(),
            1
        );
        assert!(parse_index(b"{}").is_err());
    }

    /// A repository holding a plugin called `name` whose code is
    /// `plugin.js`, with `extra` merged into its manifest.
    fn code_repo(root: &Path, name: &str, extra: Value) -> PathBuf {
        let repo = root.join(format!("{name}-source"));
        let mut manifest = serde_json::json!({
            "name": name, "version": "1.0.0", "main": "plugin.js",
            "contributes": {"tools": [{"name": "count", "description": "Counts words."}]},
        });
        for (key, value) in extra.as_object().into_iter().flatten() {
            manifest[key] = value.clone();
        }
        write(&repo.join(MANIFEST), manifest.to_string().as_bytes());
        write(
            &repo.join("plugin.js"),
            b"letronna.tool('count', () => '1');",
        );
        commit(&repo);
        repo
    }

    #[test]
    fn a_plugin_without_code_keeps_the_consent_it_had() {
        // The key as it was made before plugins ran code, so a consent the
        // user gave then still holds.
        let before = |entries: Vec<u8>| {
            let hash = ring::digest::digest(&ring::digest::SHA256, &entries);
            let hex: String = hash.as_ref()[..8]
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect();
            format!("2.0#{hex}")
        };
        let servers: Manifest = serde_json::from_value(serde_json::json!({
            "name": "kit", "version": "2.0",
            "mcpServers": {"files": {"command": "node", "args": ["server.js"]}},
        }))
        .unwrap();
        let record = Record::default();
        assert_eq!(
            consent_key(Path::new("."), &servers, &record),
            before(serde_json::to_vec(&servers.servers).unwrap())
        );
        let both: Manifest = serde_json::from_value(serde_json::json!({
            "name": "kit", "version": "2.0",
            "mcpServers": {"files": {"command": "node"}},
            "providers": [{"name": "Acme", "base": "https://api.acme.test/v1"}],
        }))
        .unwrap();
        assert_eq!(
            consent_key(Path::new("."), &both, &record),
            before(serde_json::to_vec(&(&both.servers, &both.providers)).unwrap())
        );
    }

    #[test]
    fn what_is_allowed_of_code_does_not_hang_on_the_order_its_manifest_is_written_in() {
        let read = |text: &str| -> Manifest { serde_json::from_str(text).unwrap() };
        let folder = std::env::temp_dir().join(format!("letronna-order-{}", std::process::id()));
        write(&folder.join("plugin.js"), b"");
        let checked = |mut manifest: Manifest| {
            manifest.checked = code::checked("kit", &folder, &manifest.code).unwrap();
            consent_key(&folder, &manifest, &Record::default())
        };
        let one = read(
            r#"{"name": "kit", "main": "plugin.js", "contributes": {"tools": [{"name": "t",
                "description": "d", "parameters": {"type": "object", "properties": {"a": {}}}}]}}"#,
        );
        let other = read(
            r#"{"contributes": {"tools": [{"parameters": {"properties": {"a": {}}, "type": "object"},
                "description": "d", "name": "t"}]}, "main": "plugin.js", "name": "kit"}"#,
        );
        assert_eq!(checked(one), checked(other));
        assert_eq!(
            in_order(serde_json::json!({"b": [{"d": 1, "c": 2}], "a": 0})).to_string(),
            r#"{"a":0,"b":[{"c":2,"d":1}]}"#
        );
        let _ = std::fs::remove_dir_all(&folder);
    }

    #[test]
    fn a_plugins_code_waits_for_allow_and_asks_again_when_it_changes() {
        let data = scratch("code-consent");
        let repo = code_repo(&data, "words", serde_json::json!({}));
        let plugin = add_in(&data, &source(&repo)).unwrap();
        assert!(!plugin.allowed, "code is a program, so it waits");
        let code = plugin.code.unwrap();
        assert_eq!(code.runtime, code::Runtime::JavaScript);
        assert_eq!(code.main, "plugin.js");
        assert!(code.consent().contains("JavaScript sandbox"));
        allow_in(&data, "words").unwrap();
        assert!(plugin_in(&data, "words").unwrap().allowed);

        // Same manifest, other code: a new question.
        write(
            &repo.join("plugin.js"),
            b"letronna.tool('count', () => '2');",
        );
        commit(&repo);
        update_in(&data, "words", || {}).unwrap().unwrap();
        assert!(!plugin_in(&data, "words").unwrap().allowed);
        allow_in(&data, "words").unwrap();

        // Same code, more reach: a new question too.
        let manifest = serde_json::json!({
            "name": "words", "version": "1.0.0", "main": "plugin.js",
            "permissions": ["network"],
            "contributes": {"tools": [{"name": "count", "description": "Counts words."}]},
        });
        write(&repo.join(MANIFEST), manifest.to_string().as_bytes());
        commit(&repo);
        update_in(&data, "words", || {}).unwrap().unwrap();
        let plugin = plugin_in(&data, "words").unwrap();
        assert!(!plugin.allowed);
        assert!(
            plugin
                .code
                .unwrap()
                .consent()
                .contains("reach the internet")
        );
    }

    #[test]
    fn the_users_own_edits_to_a_plugin_they_put_in_place_are_not_asked_about_again() {
        let data = scratch("code-own");
        let folder = data.join(PLUGINS_DIR).join("mine");
        let manifest = serde_json::json!({
            "name": "mine", "main": "plugin.py",
            "contributes": {"actions": [{"name": "hello"}]},
        });
        write(&folder.join(MANIFEST), manifest.to_string().as_bytes());
        write(&folder.join("plugin.py"), b"print('one')");
        assert!(!plugin_in(&data, "mine").unwrap().allowed);
        allow_in(&data, "mine").unwrap();
        write(&folder.join("plugin.py"), b"print('two')");
        let plugin = plugin_in(&data, "mine").unwrap();
        assert!(plugin.allowed);
        assert_eq!(plugin.code.unwrap().runtime, code::Runtime::Python);
    }

    #[test]
    fn a_manifest_whose_code_cannot_be_used_says_why() {
        let data = scratch("code-refused");
        let refused = |extra: Value, why: &str| {
            let folder = data.join(PLUGINS_DIR).join("bad");
            let _ = std::fs::remove_dir_all(&folder);
            let mut manifest = serde_json::json!({"name": "bad", "main": "plugin.js"});
            for (key, value) in extra.as_object().into_iter().flatten() {
                manifest[key] = value.clone();
            }
            write(&folder.join(MANIFEST), manifest.to_string().as_bytes());
            write(&folder.join("plugin.js"), b"");
            let Err(error) = read_manifest(&folder) else {
                panic!("{extra} was read");
            };
            assert!(error.contains(why), "{error}");
        };
        refused(
            serde_json::json!({"main": "../plugin.js"}),
            "not a folder inside",
        );
        refused(
            serde_json::json!({"main": "/tmp/plugin.js"}),
            "absolute path",
        );
        refused(serde_json::json!({"main": "plugin.rb"}), ".js or a .py");
        refused(serde_json::json!({"main": "other.js"}), "is not there");
        refused(serde_json::json!({"permissions": ["files"]}), "can ask for");
        refused(
            serde_json::json!({"contributes": {"events": ["window.focus"]}}),
            "the events are",
        );
        refused(
            serde_json::json!({
                "name": "a-plugin-whose-name-is-long",
                "contributes": {"tools": [{"name": "x".repeat(30), "description": "d"}]},
            }),
            "longer than the 64",
        );
        refused(
            serde_json::json!({"contributes": {"tools": [{"name": "t"}]}}),
            "no description",
        );
        refused(
            serde_json::json!({"contributes": {"tools": [
                {"name": "t", "description": "d"}, {"name": "T", "description": "d"},
            ]}}),
            "Two of its tools",
        );
        refused(
            serde_json::json!({"contributes": {"settings": [{"key": "k", "type": "choice"}]}}),
            "nothing to choose",
        );
        refused(
            serde_json::json!({"main": "", "contributes": {"status": true}}),
            "names no main file",
        );
    }

    #[test]
    fn a_plugin_holding_more_code_than_a_plugin_may_is_not_installed() {
        let data = scratch("code-big");
        let repo = code_repo(&data, "heavy", serde_json::json!({}));
        let part = vec![b'/'; 1_500_000];
        for n in 0..3 {
            write(&repo.join(format!("lib/part{n}.js")), &part);
        }
        commit(&repo);
        let error = add_in(&data, &source(&repo)).unwrap_err();
        assert!(error.contains("bytes of code"), "{error}");
        assert!(plugin_folders(&data).is_empty());
    }

    #[test]
    fn a_source_is_read_as_a_url_a_path_or_owner_and_repo() {
        assert_eq!(
            git_url("someone/kit").unwrap(),
            "https://github.com/someone/kit.git"
        );
        assert_eq!(
            git_url("https://example.com/kit.git").unwrap(),
            "https://example.com/kit.git"
        );
        assert!(git_url("http://example.com/kit.git").is_err());
        assert!(git_url("--upload-pack=touch").is_err());
        assert!(git_url("a/b/c").is_err());
    }
}
