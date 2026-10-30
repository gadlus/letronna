//! Where the keys and tokens are kept: the system's own vault where there is
//! one, the private files they have always been in where there is not.
//!
//! Provider, search and browser keys, the sign-in token and the chat apps'
//! bot tokens go to the macOS Keychain (generic passwords under the service
//! "Letronna", named for the secret) or to Windows Credential Manager
//! (generic credentials named "Letronna/<name>"). Linux keeps the files:
//! its Secret Service needs an async D-Bus client the terminal would carry
//! for nothing else. MCP servers' headers and variables stay in `mcp.json`,
//! which people write by hand.
//!
//! On macOS every entry is read, saved and removed through Apple's own
//! `security` tool, so it trusts that one program rather than the build
//! that saved it. Letronna's builds are signed ad hoc, which names each by
//! the hash of its code, so an entry the window saved asked again, entry by
//! entry, of the terminal and of every later build, however often it was
//! allowed. Through the tool no build asks. The trade is that
//! any program running as the user can read them through the tool too, as
//! it could the files before; they stay encrypted, and locked with the
//! login. An entry an older build saved asks once more, for the tool, and
//! no more once it is always allowed or saved again.
//!
//! The files still say what there is. `vault.json` records the service the
//! entries are filed under and lists each name the vault holds with a stamp
//! that changes on every write, so a secret is read from the vault once and
//! again only after the other program wrote it. A secret still in a file is
//! moved at start: written, read back, listed, and only then taken out of
//! the file, and a copy left in a file wins until then. A `vault.json` of
//! another service is not this run's to read or add to, so a scratch run
//! can never move the files' secrets where the installed Letronna does not
//! look. `keys_in: "files"` in the settings moves every secret back into
//! its file, the way back for an older Letronna.
//!
//! A vault that will not answer -- locked, or access refused -- is never
//! written around: what it holds reads as missing and `trouble` says why, a
//! write or a removal is refused rather than done in a file, and nothing is
//! cleared because it could not be read.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// A store of named secrets.
pub trait Vault: Send + Sync {
    /// The secret kept under `name`, or `None` when there is none.
    fn get(&self, name: &str) -> Result<Option<String>, String>;
    /// Keep `value` under `name`, in place of what was there.
    fn set(&self, name: &str, value: &str) -> Result<(), String>;
    /// Forget `name`; one that is not there is forgotten already.
    fn delete(&self, name: &str) -> Result<(), String>;
    /// Every name kept under the service, read from the vault's own list.
    fn names(&self) -> Result<Vec<String>, String>;
}

/// The service entries are filed under, and so what a user finds them by in
/// Keychain Access or Credential Manager.
const SERVICE: &str = "Letronna";

/// A debug build reaches a vault only under the service this names, never
/// `SERVICE`, and only with a scratch data folder, so no hand test touches
/// the real entries or the real files.
const SCRATCH_SERVICE: &str = "LETRONNA_VAULT_SERVICE";

/// Names in `keys.json` go to the vault with this in front of them.
pub(crate) const KEYS: &str = "keys.";

/// The `keys_in` setting that keeps every secret in its file.
const FILES: &str = "files";

/// A read the vault refused is tried again only after this long: redact
/// looks the keys up for every tool result, and a locked Keychain need not
/// be asked each time, while one unlocked meanwhile is read within a turn.
const RETRY: Duration = Duration::from_secs(10);

/// What to do about a vault that would not give a secret. On macOS the
/// dialog asks for the tool Letronna reads the Keychain through.
#[cfg(target_os = "macos")]
const ALLOW: &str = "choose Always Allow when it asks for \"security\"";
#[cfg(not(target_os = "macos"))]
const ALLOW: &str = "allow Letronna";

/// What to call the system's vault when none is open.
#[cfg(target_os = "macos")]
const LABEL: &str = "the Keychain";
#[cfg(windows)]
const LABEL: &str = "Credential Manager";
#[cfg(not(any(target_os = "macos", windows)))]
const LABEL: &str = "the Keychain or Credential Manager";

/// What one read of the vault gave, the stamp its name was listed with, and
/// when it was read.
struct Read {
    stamp: u64,
    got: Result<Option<String>, String>,
    at: Instant,
}

/// The vault this process uses, and what it has read from it.
struct Open {
    vault: Arc<dyn Vault>,
    /// What to call it in a sentence.
    label: &'static str,
    /// The service its entries are filed under.
    service: String,
    /// Each name's last read.
    seen: Mutex<HashMap<String, Read>>,
}

impl Open {
    fn new(label: &'static str, service: &str, vault: Arc<dyn Vault>) -> Self {
        Open {
            vault,
            label,
            service: service.to_owned(),
            seen: Mutex::new(HashMap::new()),
        }
    }

    /// What the vault holds under `name`, listed with `stamp`: read once, and
    /// again once the stamp has changed or, after a refusal, once `RETRY`
    /// has passed, so unlocking needs no restart.
    fn read(&self, name: &str, stamp: u64) -> Result<Option<String>, String> {
        if let Some(read) = self.seen().get(name)
            && read.stamp == stamp
            && (read.got.is_ok() || read.at.elapsed() < RETRY)
        {
            return read.got.clone();
        }
        // Not under the lock: a Keychain read can wait on a dialog.
        let got = self.vault.get(name);
        self.remember(name, stamp, got.clone());
        got
    }

    fn remember(&self, name: &str, stamp: u64, got: Result<Option<String>, String>) {
        let at = Instant::now();
        self.seen().insert(name.to_owned(), Read { stamp, got, at });
    }

    fn seen(&self) -> std::sync::MutexGuard<'_, HashMap<String, Read>> {
        self.seen.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// The names `vault.json` lists and their stamps, when it lists them for
    /// this vault's service.
    fn listed(&self) -> BTreeMap<String, u64> {
        let index = Index::read();
        if index.service.is_empty() || index.service == self.service {
            index.names
        } else {
            BTreeMap::new()
        }
    }
}

/// `vault.json`: the service the vault's entries are filed under, and each
/// name it holds with its stamp.
#[derive(Default, Serialize, Deserialize)]
struct Index {
    #[serde(default)]
    service: String,
    #[serde(default)]
    names: BTreeMap<String, u64>,
}

impl Index {
    fn read() -> Index {
        crate::state::read(&index_path()).unwrap_or_default()
    }
}

static OPEN: OnceLock<Arc<Open>> = OnceLock::new();

/// Why this run keeps to the files though the folder's secrets are in a
/// vault, said once, at start.
static NOTE: OnceLock<String> = OnceLock::new();

#[cfg(test)]
thread_local! {
    /// A test's own vault, so no test reaches the system's.
    static FAKE: std::cell::RefCell<Option<Arc<Open>>> = const { std::cell::RefCell::new(None) };
}

#[cfg(not(test))]
fn current() -> Option<Arc<Open>> {
    OPEN.get().cloned()
}

#[cfg(test)]
fn current() -> Option<Arc<Open>> {
    FAKE.with(|fake| fake.borrow().clone())
}

/// Keep secrets in the system's vault from now on, and move there any still
/// in a file, or with `keys_in: "files"` move them all back out. Called by
/// the two programs as they start and never by tests, so no test reaches
/// the real vault.
pub fn open() {
    let debug = cfg!(debug_assertions);
    let service = service(
        debug,
        std::env::var(SCRATCH_SERVICE).ok(),
        std::env::var_os("AGENT_DATA_DIR").is_some(),
    );
    let Some((label, vault)) = service.as_deref().and_then(platform) else {
        let index = Index::read();
        if !index.names.is_empty() {
            let _ = NOTE.set(unused(&index, debug));
        }
        return;
    };
    let service = service.unwrap_or_default();
    if let Some(other) = claim(&service) {
        let _ = NOTE.set(format!(
            "This data folder's keys and tokens are kept in {label} under \"{other}\", \
             but this run uses \"{service}\", so they read as missing here and keys \
             are read from and saved to the files."
        ));
        return;
    }
    let open = Arc::new(Open::new(label, &service, vault));
    if files_wanted() {
        if let Err(why) = move_out(&open) {
            let _ = NOTE.set(why);
        }
        return;
    }
    if OPEN.set(open.clone()).is_err() {
        return;
    }
    move_in(&open);
    // Read now, so a locked vault is said at start rather than at the
    // first request, and side by side, since each read can start a program.
    std::thread::scope(|scope| {
        for (name, stamp) in open.listed() {
            let open = &open;
            scope.spawn(move || open.read(&name, stamp));
        }
    });
}

/// The service to file entries under, or `None` to keep the files. A debug
/// build uses a vault only under a scratch name it is given and only for a
/// scratch data folder, since a scratch service over the usual folder would
/// empty its files into entries the installed Letronna never reads; a
/// release build only for the usual data folder, since two folders sharing
/// one service would write over each other's entries.
fn service(debug: bool, scratch: Option<String>, moved: bool) -> Option<String> {
    if debug {
        // Credential Manager ignores case, so "letronna" is the real one too.
        return scratch
            .map(|name| name.trim().to_owned())
            .filter(|name| moved && !name.is_empty() && !name.eq_ignore_ascii_case(SERVICE));
    }
    (!moved).then(|| SERVICE.to_owned())
}

/// Whether the settings ask for the keys to be kept in their files.
fn files_wanted() -> bool {
    crate::store::current_settings()
        .keys_in
        .trim()
        .eq_ignore_ascii_case(FILES)
}

/// Record `service` in `vault.json` as the one its names are filed under,
/// or give the other one recorded there already.
fn claim(service: &str) -> Option<String> {
    let mut other = None;
    let _ = crate::state::update::<Index>(&index_path(), |disk| {
        if !disk.is_object() {
            *disk = Value::Object(Default::default());
        }
        let Some(index) = disk.as_object_mut() else {
            return;
        };
        match index.get("service").and_then(Value::as_str) {
            Some(recorded) if !recorded.is_empty() && recorded != service => {
                other = Some(recorded.to_owned());
            }
            _ => {
                index.insert("service".into(), service.into());
            }
        }
    });
    other
}

/// Why the secrets `index` lists read as missing in a run with no vault.
fn unused(index: &Index, debug: bool) -> String {
    let why = if debug {
        "a debug build uses one only with LETRONNA_VAULT_SERVICE and AGENT_DATA_DIR both set"
    } else if std::env::var_os("AGENT_DATA_DIR").is_some() {
        "AGENT_DATA_DIR is set"
    } else {
        "there is none here"
    };
    let service = Some(index.service.as_str())
        .filter(|service| !service.is_empty())
        .unwrap_or(SERVICE);
    format!(
        "This data folder's keys and tokens are kept in {LABEL} under \"{service}\", \
         which this run does not use ({why}), so they read as missing here."
    )
}

#[cfg(target_os = "macos")]
fn platform(service: &str) -> Option<(&'static str, Arc<dyn Vault>)> {
    let service = service.to_owned();
    Some((LABEL, Arc::new(keychain::Keychain { service })))
}

#[cfg(windows)]
fn platform(service: &str) -> Option<(&'static str, Arc<dyn Vault>)> {
    let service = service.to_owned();
    Some((LABEL, Arc::new(wincred::WinCred { service })))
}

#[cfg(not(any(target_os = "macos", windows)))]
fn platform(_service: &str) -> Option<(&'static str, Arc<dyn Vault>)> {
    None
}

/// The list of what the vault holds: its service, and each name and stamp.
pub fn index_path() -> PathBuf {
    crate::chat::data_dir().join("vault.json")
}

/// The secret `name` from the vault: `None` when it holds none or there is
/// no vault, and an error when the vault would not give it.
pub(crate) fn get(name: &str) -> Result<Option<String>, String> {
    let Some(open) = current() else {
        return Ok(None);
    };
    match open.listed().get(name) {
        Some(&stamp) => open.read(name, stamp),
        None => Ok(None),
    }
}

/// Whether the vault gives a key for `slot` of `keys.json`.
pub fn has_key(slot: &str) -> bool {
    get(&format!("{KEYS}{slot}")).is_ok_and(|key| key.is_some_and(|key| !key.trim().is_empty()))
}

/// Each secret the vault holds whose name starts with `prefix`, by the rest
/// of its name; one that could not be read is left out.
pub(crate) fn under(prefix: &str) -> Vec<(String, String)> {
    let Some(open) = current() else {
        return Vec::new();
    };
    open.listed()
        .into_iter()
        .filter_map(|(name, stamp)| {
            let rest = name.strip_prefix(prefix)?.to_owned();
            Some((rest, open.read(&name, stamp).ok()??))
        })
        .collect()
}

/// Save `value` as the secret `name`, whose copy in its file is `in_file`,
/// and give what the file should hold for it now: the vault takes it where
/// there is one, the file keeps it where there is not. An empty value
/// removes it, except where the vault could not be read, as a whole form
/// saved over a secret that read as missing does.
pub(crate) fn save(name: &str, value: &str, in_file: &str) -> Result<String, String> {
    let Some(open) = current() else {
        return Ok(value.to_owned());
    };
    let now = if in_file.is_empty() {
        get(name)
    } else {
        Ok(Some(in_file.to_owned()))
    };
    match now {
        Ok(now) if now.as_deref().unwrap_or_default() == value => Ok(in_file.to_owned()),
        // What could not be read is not cleared: it may be all that is left.
        Err(_) if value.is_empty() => Ok(in_file.to_owned()),
        _ if value.is_empty() => forget(&open, name).map(|()| String::new()),
        _ => keep(&open, name, value).map(|()| String::new()),
    }
}

/// Take the secret `name` out of the vault, listed or not, as removing a key
/// or signing out does; a vault that refuses is the error, and the name
/// stays listed.
pub(crate) fn remove(name: &str) -> Result<(), String> {
    match current() {
        Some(open) => forget(&open, name),
        None => Ok(()),
    }
}

/// The vault's name, such as the login Keychain, when this run has one.
pub(crate) fn label() -> Option<String> {
    current().map(|open| open.label.to_string())
}

/// Where the keys and tokens are kept in this run, as doctor says it.
pub fn place() -> String {
    match current() {
        Some(open) => format!(
            "in {} under \"{}\", listed in vault.json",
            open.label, open.service
        ),
        None if files_wanted() => {
            "in keys.json, account.json and messaging.json (keys_in is \"files\")".into()
        }
        None => "in keys.json, account.json and messaging.json".into(),
    }
}

/// Why saved keys or tokens read as missing, once one does: a vault that
/// would not give them, one that no longer has them, or a run that does not
/// use the vault they are in.
pub fn trouble() -> Option<String> {
    if let Some(note) = NOTE.get() {
        return Some(note.clone());
    }
    let open = current()?;
    let mut refused = None;
    let mut gone = Vec::new();
    {
        let seen = open.seen();
        for (name, stamp) in open.listed() {
            let Some(read) = seen.get(&name).filter(|read| read.stamp == stamp) else {
                continue;
            };
            match &read.got {
                Err(why) => {
                    refused.get_or_insert_with(|| why.clone());
                }
                Ok(None) => gone.push(what(&name)),
                Ok(Some(_)) => {}
            }
        }
    }
    let mut label = open.label.to_owned();
    if let Some(first) = label.get_mut(..1) {
        first.make_ascii_uppercase();
    }
    let refused = refused.map(|why| {
        format!(
            "{label} would not give Letronna its saved keys ({why}). They are still \
             there: unlock it or {ALLOW}, and they are read again."
        )
    });
    let gone = (!gone.is_empty()).then(|| {
        let them = if gone.len() == 1 { "it" } else { "them" };
        format!(
            "{label} no longer has {}; enter {them} again.",
            gone.join(", ")
        )
    });
    match (refused, gone) {
        (Some(refused), Some(gone)) => Some(format!("{refused} {gone}")),
        (refused, gone) => refused.or(gone),
    }
}

/// What the secret `name` is, as a person would say it.
fn what(name: &str) -> String {
    if let Some(slot) = name.strip_prefix(KEYS) {
        return format!("your {slot} key");
    }
    if name.starts_with("account.") {
        return "your sign-in".into();
    }
    if name.starts_with("wallet.") {
        return "a wallet's key".into();
    }
    if name.starts_with("phone.") {
        return "this computer's key for your phones".into();
    }
    match name
        .strip_prefix("messaging.")
        .and_then(|rest| rest.split_once('.'))
    {
        Some((app, field)) => {
            let mut app = app.to_owned();
            if let Some(first) = app.get_mut(..1) {
                first.make_ascii_uppercase();
            }
            let kind = if field == "password" {
                "password"
            } else {
                "token"
            };
            format!("your {app} {kind}")
        }
        None => name.to_owned(),
    }
}

/// Write `value` to the vault under `name`, read it back, and only then list
/// it with a new stamp, so the other program reads it again.
fn keep(open: &Open, name: &str, value: &str) -> Result<(), String> {
    let refused = |why: String| format!("{} refused it ({why})", open.label);
    open.vault.set(name, value).map_err(refused)?;
    match open.vault.get(name) {
        Ok(Some(back)) if back == value => {}
        Ok(_) => return Err(refused("it read back different".into())),
        Err(why) => return Err(refused(why)),
    }
    let stamp = relist(open, name, true)?;
    open.remember(name, stamp, Ok(Some(value.to_owned())));
    Ok(())
}

/// Delete `name` from the vault, then from the list. Deleted whether or not
/// it is listed: one a crash or a lost `vault.json` left unlisted is still
/// the vault's, and a name not there is deleted already.
fn forget(open: &Open, name: &str) -> Result<(), String> {
    open.vault
        .delete(name)
        .map_err(|why| format!("{} refused it ({why})", open.label))?;
    relist(open, name, false)?;
    open.seen().remove(name);
    Ok(())
}

/// List `name` with a stamp none of its earlier ones had, or take it off
/// the list, and give the stamp.
fn relist(open: &Open, name: &str, listed: bool) -> Result<u64, String> {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| since.as_nanos() as u64);
    let mut stamp = now;
    crate::state::update::<Index>(&index_path(), |disk| {
        if !disk.is_object() {
            *disk = Value::Object(Default::default());
        }
        let Some(index) = disk.as_object_mut() else {
            return;
        };
        if index
            .get("service")
            .and_then(Value::as_str)
            .is_none_or(str::is_empty)
        {
            index.insert("service".into(), open.service.clone().into());
        }
        let names = index
            .entry("names")
            .or_insert_with(|| Value::Object(Default::default()));
        let Some(names) = names.as_object_mut() else {
            return;
        };
        let before = names.get(name).and_then(Value::as_u64);
        if listed {
            // A clock set back must not give a stamp this name had before.
            stamp = before.map_or(now, |before| now.max(before.saturating_add(1)));
            names.insert(name.to_owned(), stamp.into());
        } else {
            names.remove(name);
        }
    })?;
    Ok(stamp)
}

/// Move every secret still in a file into the vault. One the vault refuses
/// stays where it is, to be moved on a later start.
fn move_in(open: &Open) {
    let keys = crate::store::keys_path();
    let in_keys: HashMap<String, String> = crate::state::read(&keys).unwrap_or_default();
    for slot in in_keys.into_keys() {
        // Under the keys file's lock, as a save is, and with the slot as it
        // is then, so a key the other program saves meanwhile is not
        // replaced by the older one read above.
        let _ = crate::state::update::<HashMap<String, String>>(&keys, |disk| {
            let Some(keys) = disk.as_object_mut() else {
                return;
            };
            let Some(key) = keys.get(&slot).and_then(Value::as_str) else {
                return;
            };
            if !key.is_empty() && keep(open, &format!("{KEYS}{slot}"), key).is_ok() {
                keys.remove(&slot);
            }
        });
    }
    move_fields(open, &crate::auth::path(), crate::auth::SECRETS);
    move_fields(open, &crate::messaging::path(), crate::messaging::SECRETS);
    move_fields(open, &crate::phone::path(), crate::phone::SECRETS);
}

/// Move the secrets at `fields` of the JSON file `path` into the vault, each
/// left empty in the file once the vault has it.
fn move_fields(open: &Open, path: &Path, fields: &[(&str, &str)]) {
    let Some(json) = std::fs::read(path)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
    else {
        return;
    };
    let mut moved = Vec::new();
    for (pointer, name) in fields {
        let Some(secret) = json.pointer(pointer).and_then(Value::as_str) else {
            continue;
        };
        if !secret.is_empty() && keep(open, name, secret).is_ok() {
            moved.push((*pointer, secret.to_owned()));
        }
    }
    if moved.is_empty() {
        return;
    }
    // Under the file's lock and from what it holds now, not the copy read
    // before the vault was asked, which can wait on a dialog: the other
    // program may have saved a pairing or a new sign-in meanwhile. A field
    // is emptied only while it still holds what was moved.
    let _ = crate::state::update::<Value>(path, |disk| {
        for (pointer, secret) in &moved {
            if let Some(field) = disk.pointer_mut(pointer)
                && field.as_str() == Some(secret.as_str())
            {
                *field = Value::from("");
            }
        }
    });
}

/// Fill in each secret at `fields` of `json` that its file has no copy of
/// from the vault.
pub(crate) fn fill(json: &mut Value, fields: &[(&str, &str)]) {
    let Some(open) = current() else {
        return;
    };
    let index = open.listed();
    for (pointer, name) in fields {
        let (Some(field), Some(&stamp)) = (json.pointer_mut(pointer), index.get(*name)) else {
            continue;
        };
        if field.as_str().is_some_and(|text| !text.is_empty()) {
            continue;
        }
        if let Ok(Some(secret)) = open.read(name, stamp) {
            *field = Value::from(secret);
        }
    }
}

/// Save each secret at `fields` of `json` as `save` does, leaving in its
/// place what the file should hold; `on_disk` is the file as it is now. A
/// refused one keeps the file's copy, and the first refusal is the error.
pub(crate) fn split(
    json: &mut Value,
    on_disk: &Value,
    fields: &[(&str, &str)],
) -> Result<(), String> {
    let mut refused = Ok(());
    for (pointer, name) in fields {
        let in_file = on_disk
            .pointer(pointer)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        let Some(field) = json.pointer_mut(pointer) else {
            continue;
        };
        let value = field.as_str().unwrap_or_default().to_owned();
        let keep = save(name, &value, &in_file).unwrap_or_else(|why| {
            if refused.is_ok() {
                refused = Err(why);
            }
            in_file
        });
        *field = Value::from(keep);
    }
    refused
}

/// Move every secret the vault holds back into its file, for `keys_in:
/// "files"` and an older Letronna after it. Each is written, read back and
/// compared, and only then deleted from the vault; one that cannot be moved
/// stays there, listed, and is said.
fn move_out(open: &Open) -> Result<(), String> {
    let mut names: BTreeSet<String> = open.listed().into_keys().collect();
    // The vault's own list as well, so an entry a lost `vault.json` no
    // longer lists comes back too.
    names.extend(open.vault.names().unwrap_or_default());
    let left: Vec<String> = names
        .into_iter()
        .filter_map(|name| {
            let why = move_back(open, &name).err()?;
            Some(format!("{} ({why})", what(&name)))
        })
        .collect();
    if left.is_empty() {
        return Ok(());
    }
    let mut label = open.label.to_owned();
    if let Some(first) = label.get_mut(..1) {
        first.make_ascii_uppercase();
    }
    Err(format!(
        "{label} still has {}, which could not be moved back to the files, so it \
         reads as missing until a later start moves it.",
        left.join(", ")
    ))
}

/// Move the secret `name` out of the vault into its file. A copy the file
/// has already wins, as it does when reading, and is kept.
fn move_back(open: &Open, name: &str) -> Result<(), String> {
    let (path, pointer) = home(name).ok_or("Letronna has no file for it")?;
    if let Some(secret) = open.vault.get(name)?.filter(|secret| !secret.is_empty()) {
        let mut own = false;
        crate::state::update::<Value>(&path, |disk| {
            own = disk
                .pointer(&pointer)
                .and_then(Value::as_str)
                .is_some_and(|copy| !copy.is_empty());
            if !own {
                put(disk, &pointer, Value::from(secret.as_str()));
            }
        })?;
        let back = std::fs::read(&path)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok());
        let back = back
            .as_ref()
            .and_then(|json| json.pointer(&pointer)?.as_str());
        let kept = if own {
            back.is_some_and(|copy| !copy.is_empty())
        } else {
            back == Some(secret.as_str())
        };
        if !kept {
            return Err("its file did not keep it".into());
        }
    }
    open.vault.delete(name)?;
    relist(open, name, false)?;
    Ok(())
}

/// The file the secret `name` belongs in, and where in it.
fn home(name: &str) -> Option<(PathBuf, String)> {
    if let Some(slot) = name.strip_prefix(KEYS) {
        let slot = slot.replace('~', "~0").replace('/', "~1");
        return Some((crate::store::keys_path(), format!("/{slot}")));
    }
    [
        (crate::auth::path(), crate::auth::SECRETS),
        (crate::messaging::path(), crate::messaging::SECRETS),
        (crate::phone::path(), crate::phone::SECRETS),
    ]
    .into_iter()
    .find_map(|(path, fields)| {
        let (pointer, _) = fields.iter().find(|(_, field)| *field == name)?;
        Some((path, (*pointer).to_owned()))
    })
}

/// Put `value` at `pointer` in `json`, making the objects on the way there.
fn put(json: &mut Value, pointer: &str, value: Value) {
    let Some((parent, key)) = pointer.rsplit_once('/') else {
        return;
    };
    if json.pointer(parent).is_none_or(|here| !here.is_object()) {
        if parent.is_empty() {
            *json = Value::Object(Default::default());
        } else {
            put(json, parent, Value::Object(Default::default()));
        }
    }
    if let Some(Value::Object(here)) = json.pointer_mut(parent) {
        here.insert(key.replace("~1", "/").replace("~0", "~"), value);
    }
}

#[cfg(target_os = "macos")]
mod keychain {
    use std::io::Write;
    use std::process::{Command, Output, Stdio};
    use std::sync::{Mutex, MutexGuard};

    use security_framework::base::Error;
    use security_framework::item::{ItemClass, ItemSearchOptions, Limit};
    use security_framework::os::macos::keychain::{KeychainUserInteractionLock, SecKeychain};
    use security_framework::passwords::{PasswordOptions, generic_password};

    /// errSecItemNotFound: nothing is kept under that name.
    const NOT_FOUND: i32 = -25300;
    /// errSecInteractionNotAllowed: the Keychain is locked and may not ask.
    const LOCKED: i32 = -25308;
    /// errSecAuthFailed and userCanceledErr: the person said no.
    const REFUSED: [i32; 2] = [-25293, -128];

    /// Apple's own Keychain tool, the one program every entry trusts.
    const TOOL: &str = "/usr/bin/security";

    /// The longest line `security -i` reads whole; a longer one is cut, and
    /// what is left of it saved as though it were the secret.
    const LINE: usize = 4096;

    /// A debug or test build raises no dialog: a hand test must not put one
    /// on the desktop, so a locked Keychain just says it is locked.
    const QUIET: bool = cfg!(any(debug_assertions, test));

    /// The login Keychain: a generic password per secret, filed under the
    /// service, with the secret's name as its account.
    pub(super) struct Keychain {
        pub(super) service: String,
    }

    impl super::Vault for Keychain {
        fn get(&self, name: &str) -> Result<Option<String>, String> {
            match quietly(&self.service, name) {
                Ok(bytes) => return text(bytes).map(Some),
                Err(NOT_FOUND) => return Ok(None),
                Err(LOCKED) if QUIET => return Err("it is locked".into()),
                // The tool's, or another build's: the tool reads either.
                Err(_) => {}
            }
            let out = tool(
                &[
                    "find-generic-password",
                    "-s",
                    &self.service,
                    "-a",
                    name,
                    "-g",
                ],
                "",
            )?;
            if exited(&out, NOT_FOUND) {
                return Ok(None);
            }
            if !out.status.success() {
                return Err(failed(&out));
            }
            let said = String::from_utf8_lossy(&out.stderr);
            let shown = said
                .lines()
                .find_map(|line| line.strip_prefix("password:"))
                .ok_or("it gave no secret")?;
            text(shown_as(shown.trim_start())?).map(Some)
        }

        fn set(&self, name: &str, value: &str) -> Result<(), String> {
            unlocked(&self.service, name)?;
            let (service, name) = (quoted(&self.service)?, quoted(name)?);
            // On the tool's input rather than its arguments, which `ps`
            // shows to every account on this Mac.
            let secret = if value.bytes().all(|byte| (b' '..=b'~').contains(&byte)) {
                format!("-w {}", quoted(value)?)
            } else {
                format!("-X {}", hex(value.as_bytes()))
            };
            let add = format!("add-generic-password -s {service} -a {name} {secret}");
            if add.len() >= LINE {
                return Err("it is too long for the Keychain".into());
            }
            // Removed and saved anew rather than updated, so the entry is the
            // tool's whichever build saved it before.
            let input = format!("delete-generic-password -s {service} -a {name}\n{add}\n");
            let out = tool(&["-i"], &input)?;
            if out.status.success() {
                Ok(())
            } else {
                Err(failed(&out))
            }
        }

        fn delete(&self, name: &str) -> Result<(), String> {
            unlocked(&self.service, name)?;
            let out = tool(
                &["delete-generic-password", "-s", &self.service, "-a", name],
                "",
            )?;
            if out.status.success() || exited(&out, NOT_FOUND) {
                Ok(())
            } else {
                Err(failed(&out))
            }
        }

        fn names(&self) -> Result<Vec<String>, String> {
            let _hush = QUIET.then(hush);
            // Attributes only: a list reads no secret, so it asks nothing.
            let found = ItemSearchOptions::new()
                .class(ItemClass::generic_password())
                .service(&self.service)
                .load_attributes(true)
                .limit(Limit::All)
                .search();
            match found {
                Ok(found) => Ok(found
                    .iter()
                    .filter_map(|item| item.simplify_dict()?.remove("acct"))
                    .collect()),
                Err(error) if error.code() == NOT_FOUND => Ok(Vec::new()),
                Err(error) => Err(said(error)),
            }
        }
    }

    /// The secret `name` as this process reads it itself, raising no
    /// dialog. An entry the tool saved trusts only the tool, so this mostly
    /// finds out that there is none, which then needs no tool started, or
    /// that the Keychain is locked.
    fn quietly(service: &str, name: &str) -> Result<Vec<u8>, i32> {
        let _hush = hush();
        generic_password(PasswordOptions::new_generic_password(service, name))
            .map_err(|error| error.code())
    }

    /// In a quiet build, stop at a locked Keychain before the tool asks to
    /// unlock it.
    fn unlocked(service: &str, name: &str) -> Result<(), String> {
        if QUIET && quietly(service, name) == Err(LOCKED) {
            return Err("it is locked".into());
        }
        Ok(())
    }

    /// Run the tool with `args` and `input` written to it.
    fn tool(args: &[&str], input: &str) -> Result<Output, String> {
        let mut child = Command::new(TOOL)
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| format!("{TOOL} would not start ({e})"))?;
        if let Some(mut stdin) = child.stdin.take() {
            // One that stopped reading has said why on stderr.
            let _ = stdin.write_all(input.as_bytes());
        }
        child.wait_with_output().map_err(|e| e.to_string())
    }

    /// Whether the tool exited with `code`, which it gives as its low byte.
    fn exited(out: &Output, code: i32) -> bool {
        out.status.code() == Some(code & 0xff)
    }

    /// Why the tool failed, in the words `said` uses for the same codes.
    fn failed(out: &Output) -> String {
        if exited(out, LOCKED) {
            return "it is locked".into();
        }
        if REFUSED.iter().any(|&code| exited(out, code)) {
            return "access was not allowed".into();
        }
        // Its own words follow the name of the call that failed.
        String::from_utf8_lossy(&out.stderr)
            .lines()
            .rev()
            .find(|line| line.starts_with("security: "))
            .and_then(|line| line.rsplit(": ").next())
            .map_or_else(
                || format!("{TOOL} failed"),
                |why| why.trim().trim_end_matches('.').to_owned(),
            )
    }

    /// The secret as `-g` shows it: `"text"`, `0x<hex>  "..."` for anything
    /// but plain ASCII, or nothing at all for an empty one.
    pub(super) fn shown_as(shown: &str) -> Result<Vec<u8>, String> {
        let Some(hex) = shown.strip_prefix("0x") else {
            let text = shown
                .strip_prefix('"')
                .and_then(|text| text.strip_suffix('"'));
            return Ok(text.unwrap_or(shown).as_bytes().to_vec());
        };
        let hex = hex.split_whitespace().next().unwrap_or_default();
        unhex(hex).ok_or_else(|| "what it holds came back garbled".into())
    }

    /// `text` in double quotes as `security -i` reads it; a line break or
    /// other control character would end its line, so is refused.
    pub(super) fn quoted(text: &str) -> Result<String, String> {
        if text.chars().any(char::is_control) {
            return Err("its name has a line break or other control character in it".into());
        }
        Ok(format!(
            "\"{}\"",
            text.replace('\\', "\\\\").replace('"', "\\\"")
        ))
    }

    fn text(bytes: Vec<u8>) -> Result<String, String> {
        String::from_utf8(bytes).map_err(|_| "what it holds is not text".into())
    }

    fn hex(bytes: &[u8]) -> String {
        bytes.iter().map(|byte| format!("{byte:02x}")).collect()
    }

    fn unhex(hex: &str) -> Option<Vec<u8>> {
        if !hex.len().is_multiple_of(2) {
            return None;
        }
        (0..hex.len())
            .step_by(2)
            .map(|at| u8::from_str_radix(hex.get(at..at + 2)?, 16).ok())
            .collect()
    }

    /// While held, this process's own Keychain calls raise no dialog. The
    /// switch is the whole process's, so one hold at a time: a call ending
    /// must not let a dialog through for another still under way.
    struct Hush {
        _quiet: Option<KeychainUserInteractionLock>,
        _turn: MutexGuard<'static, ()>,
    }

    fn hush() -> Hush {
        static TURN: Mutex<()> = Mutex::new(());
        let turn = TURN.lock().unwrap_or_else(|e| e.into_inner());
        Hush {
            _quiet: SecKeychain::disable_user_interaction().ok(),
            _turn: turn,
        }
    }

    fn said(error: Error) -> String {
        match error.code() {
            LOCKED => "it is locked".into(),
            code if REFUSED.contains(&code) => "access was not allowed".into(),
            _ => error.to_string(),
        }
    }
}

#[cfg(windows)]
mod wincred {
    use windows_sys::Win32::Foundation::{ERROR_NOT_FOUND, FILETIME, GetLastError};
    use windows_sys::Win32::Security::Credentials::{
        CRED_MAX_CREDENTIAL_BLOB_SIZE, CRED_PERSIST_LOCAL_MACHINE, CRED_TYPE_GENERIC, CREDENTIALW,
        CredDeleteW, CredEnumerateW, CredFree, CredReadW, CredWriteW,
    };

    /// Credential Manager: a generic credential per secret, named for the
    /// service and the secret, kept on this machine for this user.
    pub(super) struct WinCred {
        pub(super) service: String,
    }

    impl WinCred {
        /// The credential's name, NUL-terminated UTF-16 as Windows takes it.
        fn target(&self, name: &str) -> Vec<u16> {
            format!("{}/{name}", self.service)
                .encode_utf16()
                .chain([0])
                .collect()
        }
    }

    fn failed() -> String {
        // SAFETY: reads this thread's last error; no memory is involved.
        format!("Windows error {}", unsafe { GetLastError() })
    }

    impl super::Vault for WinCred {
        fn get(&self, name: &str) -> Result<Option<String>, String> {
            let target = self.target(name);
            let mut found: *mut CREDENTIALW = std::ptr::null_mut();
            // SAFETY: the target is NUL-terminated and outlives the call.
            if unsafe { CredReadW(target.as_ptr(), CRED_TYPE_GENERIC, 0, &mut found) } == 0 {
                // SAFETY: as in `failed`.
                if unsafe { GetLastError() } == ERROR_NOT_FOUND {
                    return Ok(None);
                }
                return Err(failed());
            }
            // SAFETY: the read succeeded, so `found` is a credential whose blob
            // holds the size it gives, until it is freed below.
            let bytes = unsafe {
                let credential = &*found;
                let blob = if credential.CredentialBlob.is_null() {
                    Vec::new()
                } else {
                    let size = credential.CredentialBlobSize as usize;
                    std::slice::from_raw_parts(credential.CredentialBlob, size).to_vec()
                };
                CredFree(found.cast());
                blob
            };
            String::from_utf8(bytes)
                .map(Some)
                .map_err(|_| "what it holds is not text".into())
        }

        fn set(&self, name: &str, value: &str) -> Result<(), String> {
            if value.len() > CRED_MAX_CREDENTIAL_BLOB_SIZE as usize {
                return Err("it is too long for Credential Manager".into());
            }
            let mut target = self.target(name);
            let mut blob = value.as_bytes().to_vec();
            let credential = CREDENTIALW {
                Flags: 0,
                Type: CRED_TYPE_GENERIC,
                TargetName: target.as_mut_ptr(),
                Comment: std::ptr::null_mut(),
                LastWritten: FILETIME {
                    dwLowDateTime: 0,
                    dwHighDateTime: 0,
                },
                CredentialBlobSize: blob.len() as u32,
                CredentialBlob: blob.as_mut_ptr(),
                Persist: CRED_PERSIST_LOCAL_MACHINE,
                AttributeCount: 0,
                Attributes: std::ptr::null_mut(),
                TargetAlias: std::ptr::null_mut(),
                UserName: std::ptr::null_mut(),
            };
            // SAFETY: every pointer in the credential outlives the call.
            if unsafe { CredWriteW(&credential, 0) } == 0 {
                return Err(failed());
            }
            Ok(())
        }

        fn delete(&self, name: &str) -> Result<(), String> {
            let target = self.target(name);
            // SAFETY: the target is NUL-terminated and outlives the call.
            if unsafe { CredDeleteW(target.as_ptr(), CRED_TYPE_GENERIC, 0) } == 0 {
                // SAFETY: as in `failed`.
                if unsafe { GetLastError() } == ERROR_NOT_FOUND {
                    return Ok(());
                }
                return Err(failed());
            }
            Ok(())
        }

        fn names(&self) -> Result<Vec<String>, String> {
            let filter: Vec<u16> = format!("{}/*", self.service)
                .encode_utf16()
                .chain([0])
                .collect();
            let mut count = 0;
            let mut found: *mut *mut CREDENTIALW = std::ptr::null_mut();
            // SAFETY: the filter is NUL-terminated and outlives the call.
            if unsafe { CredEnumerateW(filter.as_ptr(), 0, &mut count, &mut found) } == 0 {
                // SAFETY: as in `failed`.
                if unsafe { GetLastError() } == ERROR_NOT_FOUND {
                    return Ok(Vec::new());
                }
                return Err(failed());
            }
            // SAFETY: the call succeeded, so `found` holds `count` credentials,
            // each named by a NUL-terminated string, until it is freed below.
            let targets: Vec<String> = unsafe {
                let targets = (0..count as usize)
                    .filter_map(|at| {
                        let target = (**found.add(at)).TargetName;
                        if target.is_null() {
                            return None;
                        }
                        let length = (0..).take_while(|&i| *target.add(i) != 0).count();
                        String::from_utf16(std::slice::from_raw_parts(target, length)).ok()
                    })
                    .collect();
                CredFree(found.cast());
                targets
            };
            let prefix = format!("{}/", self.service);
            Ok(targets
                .iter()
                .filter_map(|target| Some(target.strip_prefix(&prefix)?.to_owned()))
                .collect())
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::{
        FAKE, Index, KEYS, Open, RETRY, SERVICE, Vault, claim, keep, move_in, move_out, service,
        trouble, what,
    };
    use std::collections::BTreeMap;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};

    /// What the test vault is filed under.
    const TEST_SERVICE: &str = "Letronna-test";

    /// A vault in memory, which can be locked and counts its reads.
    #[derive(Default)]
    pub(crate) struct Fake {
        pub(crate) held: Mutex<BTreeMap<String, String>>,
        pub(crate) locked: AtomicBool,
        /// Keeps something other than it is given, as a faulty vault might.
        pub(crate) garbles: AtomicBool,
        pub(crate) reads: AtomicUsize,
        /// Run once at the next write, as the other program would meanwhile.
        pub(crate) meanwhile: Mutex<Option<Box<dyn FnOnce() + Send>>>,
    }

    impl Fake {
        pub(crate) fn holds(&self, name: &str) -> Option<String> {
            self.held.lock().unwrap().get(name).cloned()
        }

        fn refuse(&self) -> Result<(), String> {
            if self.locked.load(Ordering::SeqCst) {
                return Err("it is locked".into());
            }
            Ok(())
        }
    }

    impl Vault for Fake {
        fn get(&self, name: &str) -> Result<Option<String>, String> {
            self.reads.fetch_add(1, Ordering::SeqCst);
            self.refuse()?;
            Ok(self.holds(name))
        }

        fn set(&self, name: &str, value: &str) -> Result<(), String> {
            self.refuse()?;
            if let Some(other) = self.meanwhile.lock().unwrap().take() {
                other();
            }
            let mut value = value.to_owned();
            if self.garbles.load(Ordering::SeqCst) {
                value.push('?');
            }
            self.held.lock().unwrap().insert(name.into(), value);
            Ok(())
        }

        fn delete(&self, name: &str) -> Result<(), String> {
            self.refuse()?;
            self.held.lock().unwrap().remove(name);
            Ok(())
        }

        fn names(&self) -> Result<Vec<String>, String> {
            self.refuse()?;
            Ok(self.held.lock().unwrap().keys().cloned().collect())
        }
    }

    /// Give this test's thread an empty vault of its own, and hand it back.
    pub(crate) fn fake() -> Arc<Fake> {
        let fake = Arc::new(Fake::default());
        let open = Open::new("the test vault", TEST_SERVICE, fake.clone());
        FAKE.with(|slot| *slot.borrow_mut() = Some(Arc::new(open)));
        fake
    }

    /// The same vault as the next start of the program sees it: nothing read.
    pub(crate) fn restart(fake: &Arc<Fake>) {
        let open = Open::new("the test vault", TEST_SERVICE, fake.clone());
        FAKE.with(|slot| *slot.borrow_mut() = Some(Arc::new(open)));
    }

    /// A data folder of its own for one test, set as `AGENT_DATA_DIR`.
    fn data(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("letronna-vault-{name}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        unsafe { std::env::set_var("AGENT_DATA_DIR", &dir) };
        dir
    }

    /// Age every read this program has made, so a refused one is asked again.
    fn wait_out(open: &Open) {
        for read in open.seen().values_mut() {
            read.at = read.at.checked_sub(RETRY).unwrap();
        }
    }

    /// The names `vault.json` lists.
    fn listed() -> BTreeMap<String, u64> {
        Index::read().names
    }

    /// The test's vault as this program has it open.
    fn opened() -> Arc<Open> {
        FAKE.with(|slot| slot.borrow().clone()).unwrap()
    }

    /// The JSON file at `path`, or null when there is none.
    fn read(path: std::path::PathBuf) -> serde_json::Value {
        std::fs::read(path).map_or(serde_json::Value::Null, |bytes| {
            serde_json::from_slice(&bytes).unwrap()
        })
    }

    #[test]
    fn a_debug_build_uses_a_vault_only_under_a_scratch_name_for_a_scratch_folder() {
        let scratch = Some("Letronna-test-7".to_owned());
        assert_eq!(
            service(true, scratch.clone(), true).as_deref(),
            Some("Letronna-test-7")
        );
        // Over the usual data folder it would empty the real files into
        // entries the installed Letronna never reads.
        assert_eq!(service(true, scratch.clone(), false), None);
        assert_eq!(service(true, None, true), None);
        assert_eq!(service(true, Some(SERVICE.into()), true), None);
        // Credential Manager ignores case.
        assert_eq!(service(true, Some("letronna".into()), true), None);
        assert_eq!(service(true, Some(" LETRONNA ".into()), true), None);
        assert_eq!(service(true, Some(" ".into()), true), None);
        // A release build files under the real name, but only for its own
        // data folder, and takes no scratch name.
        assert_eq!(
            service(false, scratch.clone(), false).as_deref(),
            Some(SERVICE)
        );
        assert_eq!(service(false, scratch, true), None);
    }

    #[test]
    fn a_key_saved_with_a_vault_is_kept_there_and_not_in_the_file() {
        let _turn = crate::store::tests::DATA_DIR
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let dir = data("saved");
        let vault = fake();
        std::fs::write(dir.join("keys.json"), r#"{"Custom":"c-2"}"#).unwrap();

        let keys = crate::store::save_key("OpenAI", Some("sk-live-1")).unwrap();
        assert_eq!(vault.holds("keys.OpenAI").as_deref(), Some("sk-live-1"));
        assert!(listed().contains_key("keys.OpenAI"));
        assert_eq!(
            read(dir.join("keys.json")),
            serde_json::json!({"Custom": "c-2"})
        );
        assert_eq!(keys["OpenAI"], "sk-live-1");
        assert_eq!(keys["Custom"], "c-2");
        assert_eq!(crate::store::load_keys(), keys);

        // Saving over the file's own copy moves that slot to the vault too.
        crate::store::save_key("Custom", Some("c-3")).unwrap();
        assert_eq!(read(dir.join("keys.json")), serde_json::json!({}));
        assert_eq!(vault.holds("keys.Custom").as_deref(), Some("c-3"));

        // Taking a key out takes it out of the vault and the list.
        let keys = crate::store::save_key("OpenAI", None).unwrap();
        assert!(!keys.contains_key("OpenAI"));
        assert_eq!(vault.holds("keys.OpenAI"), None);
        assert!(!listed().contains_key("keys.OpenAI"));
        // One left unlisted, as a crash between the write and the list
        // would leave it, is still taken out.
        vault
            .held
            .lock()
            .unwrap()
            .insert("keys.Exa".into(), "exa-1".into());
        crate::store::save_key("Exa", None).unwrap();
        assert_eq!(vault.holds("keys.Exa"), None);
        unsafe { std::env::remove_var("AGENT_DATA_DIR") };
    }

    #[test]
    fn secrets_in_files_move_to_the_vault_and_only_then_out_of_the_files() {
        let _turn = crate::store::tests::DATA_DIR
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let dir = data("move");
        let vault = fake();
        std::fs::write(
            dir.join("keys.json"),
            r#"{"OpenAI":"sk-live-0123456789","Brave":""}"#,
        )
        .unwrap();
        let account = r#"{"name":"user_a","email":"a@example.com","token":"device-token-0123456789","since":1}"#;
        std::fs::write(dir.join("account.json"), account).unwrap();
        let chats = r#"{"telegram":{"enabled":true,"token":"123:telegram-token","chat":5},
            "slack":{"app_token":"xapp-1-app-token","bot_token":"xoxb-bot-token-0123456789"}}"#;
        std::fs::write(dir.join("messaging.json"), chats).unwrap();

        restart(&vault);
        move_in(&opened());

        assert_eq!(
            vault.holds("keys.OpenAI").as_deref(),
            Some("sk-live-0123456789")
        );
        assert_eq!(
            vault.holds("account.token").as_deref(),
            Some("device-token-0123456789")
        );
        assert_eq!(
            vault.holds("messaging.telegram.token").as_deref(),
            Some("123:telegram-token")
        );
        assert_eq!(
            vault.holds("messaging.slack.bot_token").as_deref(),
            Some("xoxb-bot-token-0123456789")
        );
        // An empty slot is nothing to move.
        assert_eq!(vault.holds("keys.Brave"), None);
        assert_eq!(
            read(dir.join("keys.json")),
            serde_json::json!({"Brave": ""})
        );
        let account = read(dir.join("account.json"));
        assert_eq!(
            (account["token"].as_str(), account["name"].as_str()),
            (Some(""), Some("user_a"))
        );
        let chats = read(dir.join("messaging.json"));
        assert_eq!(chats["telegram"]["token"], "");
        assert_eq!(chats["telegram"]["chat"], 5);
        assert_eq!(chats["slack"]["app_token"], "");

        // Everything reads as before, from the vault, and is still scrubbed.
        restart(&vault);
        assert_eq!(crate::store::load_keys()["OpenAI"], "sk-live-0123456789");
        assert_eq!(
            crate::auth::account().unwrap().token,
            "device-token-0123456789"
        );
        let chats = crate::messaging::load();
        assert_eq!(chats.telegram.token, "123:telegram-token");
        assert_eq!(chats.slack.app_token, "xapp-1-app-token");
        let secrets = crate::redact::secrets();
        for secret in [
            "sk-live-0123456789",
            "device-token-0123456789",
            "123:telegram-token",
            "xoxb-bot-token-0123456789",
        ] {
            assert!(secrets.iter().any(|s| s == secret), "{secret} not scrubbed");
        }
        unsafe { std::env::remove_var("AGENT_DATA_DIR") };
    }

    #[test]
    fn a_secret_the_vault_does_not_read_back_stays_in_its_file() {
        let _turn = crate::store::tests::DATA_DIR
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let dir = data("garbled");
        let vault = fake();
        vault.garbles.store(true, Ordering::SeqCst);
        std::fs::write(dir.join("keys.json"), r#"{"OpenAI":"sk-live-1"}"#).unwrap();

        move_in(&opened());
        assert_eq!(
            read(dir.join("keys.json")),
            serde_json::json!({"OpenAI": "sk-live-1"})
        );
        assert!(listed().is_empty());
        // The file's copy is what is read, not the garbled one.
        assert_eq!(crate::store::load_keys()["OpenAI"], "sk-live-1");
        unsafe { std::env::remove_var("AGENT_DATA_DIR") };
    }

    #[test]
    fn a_locked_vault_reads_as_missing_says_so_and_neither_writes_nor_clears() {
        let _turn = crate::store::tests::DATA_DIR
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let dir = data("locked");
        let vault = fake();
        crate::store::save_key("OpenAI", Some("sk-live-1")).unwrap();
        let chats = crate::messaging::Config {
            telegram: crate::messaging::Telegram {
                token: "123:telegram-token".into(),
                ..Default::default()
            },
            ..Default::default()
        };
        crate::messaging::save(&chats).unwrap();
        assert_eq!(
            vault.holds("messaging.telegram.token").as_deref(),
            Some("123:telegram-token")
        );
        assert_eq!(trouble(), None);

        restart(&vault);
        vault.locked.store(true, Ordering::SeqCst);
        assert!(!crate::store::load_keys().contains_key("OpenAI"));
        assert!(trouble().is_some_and(|why| why.contains("it is locked")));
        // A new key is refused, not put in the file.
        assert!(crate::store::save_key("OpenAI", Some("sk-live-2")).is_err());
        assert!(crate::store::save_key("Custom", Some("c-1")).is_err());
        assert_eq!(read(dir.join("keys.json")), serde_json::Value::Null);
        // Settings saved over a token that read as empty leave the token be.
        let mut chats = crate::messaging::load();
        assert_eq!(chats.telegram.token, "");
        chats.telegram.enabled = true;
        crate::messaging::save(&chats).unwrap();
        // Removing a key it will not answer for is refused and said, and the
        // key stays listed.
        let removed = crate::store::save_key("OpenAI", None);
        assert!(removed.is_err_and(|why| why.to_string().contains("refused it (it is locked)")));
        assert!(listed().contains_key("keys.OpenAI"));

        // Unlocked, the same program reads it again once the refusal has
        // aged, with no restart.
        vault.locked.store(false, Ordering::SeqCst);
        assert!(!crate::store::load_keys().contains_key("OpenAI"));
        wait_out(&opened());
        assert_eq!(crate::store::load_keys()["OpenAI"], "sk-live-1");
        let chats = crate::messaging::load();
        assert!(chats.telegram.enabled);
        assert_eq!(chats.telegram.token, "123:telegram-token");
        assert_eq!(trouble(), None);
        unsafe { std::env::remove_var("AGENT_DATA_DIR") };
    }

    #[test]
    fn a_secret_is_read_once_and_again_only_after_the_other_program_wrote_it() {
        let _turn = crate::store::tests::DATA_DIR
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        data("stamps");
        let vault = fake();
        crate::store::save_key("OpenAI", Some("sk-live-1")).unwrap();
        restart(&vault);
        let before = vault.reads.load(Ordering::SeqCst);
        for _ in 0..3 {
            assert_eq!(crate::store::load_keys()["OpenAI"], "sk-live-1");
        }
        assert_eq!(vault.reads.load(Ordering::SeqCst), before + 1);

        // The other program, with a vault of its own over the same entries.
        let other = Open::new("the test vault", TEST_SERVICE, vault.clone());
        keep(&other, &format!("{KEYS}OpenAI"), "sk-live-2").unwrap();
        assert_eq!(crate::store::load_keys()["OpenAI"], "sk-live-2");
        unsafe { std::env::remove_var("AGENT_DATA_DIR") };
    }

    #[test]
    fn a_secret_the_vault_no_longer_has_is_said_rather_than_silently_missing() {
        let _turn = crate::store::tests::DATA_DIR
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        data("gone");
        let vault = fake();
        crate::store::save_key("OpenAI", Some("sk-live-1")).unwrap();
        vault.held.lock().unwrap().clear();
        restart(&vault);
        assert!(!crate::store::load_keys().contains_key("OpenAI"));
        assert!(listed().contains_key("keys.OpenAI"));
        assert_eq!(
            trouble().as_deref(),
            Some("The test vault no longer has your OpenAI key; enter it again.")
        );
        crate::store::save_key("OpenAI", Some("sk-live-2")).unwrap();
        assert_eq!(trouble(), None);
        unsafe { std::env::remove_var("AGENT_DATA_DIR") };
    }

    #[test]
    fn the_list_records_its_service_and_another_services_run_reads_none_of_it() {
        let _turn = crate::store::tests::DATA_DIR
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        data("service");
        let vault = fake();
        crate::store::save_key("OpenAI", Some("sk-live-1")).unwrap();
        assert_eq!(Index::read().service, TEST_SERVICE);
        assert_eq!(claim(TEST_SERVICE), None);
        assert_eq!(claim("Letronna-other").as_deref(), Some(TEST_SERVICE));
        assert_eq!(Index::read().service, TEST_SERVICE);

        let other = Open::new("the other vault", "Letronna-other", vault.clone());
        FAKE.with(|slot| *slot.borrow_mut() = Some(Arc::new(other)));
        assert!(!crate::store::load_keys().contains_key("OpenAI"));
        unsafe { std::env::remove_var("AGENT_DATA_DIR") };
    }

    #[test]
    fn a_pairing_saved_while_a_token_moves_is_kept() {
        let _turn = crate::store::tests::DATA_DIR
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let dir = data("pairing");
        let vault = fake();
        let path = dir.join("messaging.json");
        let chats = r#"{"telegram":{"enabled":true,"token":"123:telegram-token"}}"#;
        std::fs::write(&path, chats).unwrap();
        let other = path.clone();
        *vault.meanwhile.lock().unwrap() = Some(Box::new(move || {
            // The other program pairs a chat while this one moves the token.
            let paired = r#"{"telegram":{"enabled":true,"token":"123:telegram-token",
                "chat":42,"owner":7}}"#;
            std::fs::write(other, paired).unwrap();
        }));

        move_in(&opened());
        let chats = read(path);
        assert_eq!(chats["telegram"]["chat"], 42);
        assert_eq!(chats["telegram"]["owner"], 7);
        assert_eq!(chats["telegram"]["token"], "");
        assert_eq!(
            vault.holds("messaging.telegram.token").as_deref(),
            Some("123:telegram-token")
        );
        unsafe { std::env::remove_var("AGENT_DATA_DIR") };
    }

    #[test]
    fn keys_in_files_moves_every_secret_back_into_its_file() {
        let _turn = crate::store::tests::DATA_DIR
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let dir = data("out");
        let vault = fake();
        std::fs::write(
            dir.join("keys.json"),
            r#"{"OpenAI":"sk-live-1","Brave":"b-1"}"#,
        )
        .unwrap();
        let account = r#"{"name":"user_a","email":"a@example.com","token":"device-token-0123456789","since":1}"#;
        std::fs::write(dir.join("account.json"), account).unwrap();
        let chats = r#"{"telegram":{"token":"123:telegram-token","chat":5},
            "slack":{"bot_token":"xoxb-bot-token-0123456789"}}"#;
        std::fs::write(dir.join("messaging.json"), chats).unwrap();
        move_in(&opened());
        assert_eq!(read(dir.join("keys.json")), serde_json::json!({}));
        // One the list lost is found in the vault's own list.
        vault
            .held
            .lock()
            .unwrap()
            .insert("keys.Exa".into(), "exa-1".into());

        // A locked vault keeps them all, listed, and says which.
        vault.locked.store(true, Ordering::SeqCst);
        let why = move_out(&opened()).unwrap_err();
        assert!(why.contains("your OpenAI key (it is locked)"), "{why}");
        assert_eq!(listed().len(), 5);
        assert_eq!(read(dir.join("keys.json")), serde_json::json!({}));

        vault.locked.store(false, Ordering::SeqCst);
        move_out(&opened()).unwrap();
        assert_eq!(
            read(dir.join("keys.json")),
            serde_json::json!({"OpenAI": "sk-live-1", "Brave": "b-1", "Exa": "exa-1"})
        );
        let account = read(dir.join("account.json"));
        assert_eq!(account["token"], "device-token-0123456789");
        assert_eq!(account["name"], "user_a");
        let chats = read(dir.join("messaging.json"));
        assert_eq!(chats["telegram"]["token"], "123:telegram-token");
        assert_eq!(chats["telegram"]["chat"], 5);
        assert_eq!(chats["slack"]["bot_token"], "xoxb-bot-token-0123456789");
        assert!(vault.held.lock().unwrap().is_empty());
        assert!(listed().is_empty());

        // With no vault, as the rest of that run has, each reads from its file.
        FAKE.with(|slot| *slot.borrow_mut() = None);
        assert_eq!(crate::store::load_keys()["Exa"], "exa-1");
        assert_eq!(
            crate::auth::account().unwrap().token,
            "device-token-0123456789"
        );
        assert_eq!(
            crate::messaging::load().telegram.token,
            "123:telegram-token"
        );
        unsafe { std::env::remove_var("AGENT_DATA_DIR") };
    }

    #[test]
    fn signing_out_says_what_is_left_rather_than_leave_the_token_unseen() {
        let _turn = crate::store::tests::DATA_DIR
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let dir = data("sign-out");
        let vault = fake();
        let account = r#"{"name":"user_a","email":"a@example.com","token":"device-token-0123456789","since":1}"#;
        std::fs::write(dir.join("account.json"), account).unwrap();
        move_in(&opened());
        restart(&vault);

        // The token cannot be read, so the server is not asked, and the
        // vault will not let it go: nothing is signed out, and that is said.
        vault.locked.store(true, Ordering::SeqCst);
        let why = crate::auth::sign_out().unwrap_err();
        assert!(why.contains("it is locked"), "{why}");
        assert!(dir.join("account.json").exists());
        assert!(listed().contains_key("account.token"));

        // Unlocked with the token gone from it: signed out here, and told
        // to sign out on the website too, since the server was not told.
        vault.locked.store(false, Ordering::SeqCst);
        vault.held.lock().unwrap().clear();
        wait_out(&opened());
        let note = crate::auth::sign_out().unwrap();
        assert!(note.is_some_and(|note| note.contains("website")));
        assert!(!dir.join("account.json").exists());
        assert!(listed().is_empty());
        unsafe { std::env::remove_var("AGENT_DATA_DIR") };
    }

    /// Deletes what a Keychain test wrote, however the test ends.
    #[cfg(target_os = "macos")]
    struct Tidy<'a>(&'a super::keychain::Keychain);

    #[cfg(target_os = "macos")]
    impl Drop for Tidy<'_> {
        fn drop(&mut self) {
            let _ = self.0.delete("key");
        }
    }

    #[test]
    fn a_mail_app_password_is_kept_in_the_vault_and_hidden_like_a_token() {
        let _turn = crate::store::tests::DATA_DIR
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let dir = data("mail");
        let vault = fake();
        let chats = crate::messaging::Config {
            email: crate::messaging::Email {
                enabled: true,
                password: "abcd-efgh-ijkl-mnop".into(),
                to: "reports@example.com".into(),
                ..Default::default()
            },
            ..Default::default()
        };
        crate::messaging::save(&chats).unwrap();
        assert_eq!(
            vault.holds("messaging.email.password").as_deref(),
            Some("abcd-efgh-ijkl-mnop")
        );
        let file = read(dir.join("messaging.json"));
        assert_eq!(file["email"]["password"], "");
        assert_eq!(file["email"]["to"], "reports@example.com");

        restart(&vault);
        assert_eq!(
            crate::messaging::load().email.password,
            "abcd-efgh-ijkl-mnop"
        );
        assert!(
            crate::redact::secrets()
                .iter()
                .any(|secret| secret == "abcd-efgh-ijkl-mnop")
        );
        assert_eq!(what("messaging.email.password"), "your Email password");
        unsafe { std::env::remove_var("AGENT_DATA_DIR") };
    }

    #[cfg(target_os = "macos")]
    #[test]
    #[ignore = "writes to the login Keychain under a scratch service it deletes again: \
                cargo test -p letronna-core --release -- --ignored the_keychain"]
    fn the_keychain_keeps_reads_and_forgets_a_secret() {
        let service = format!("Letronna-test-{}", std::process::id());
        assert_ne!(service, SERVICE);
        let keychain = super::keychain::Keychain { service };
        let _tidy = Tidy(&keychain);
        assert_eq!(keychain.get("key"), Ok(None));
        keychain.set("key", "sk-test-1").unwrap();
        assert_eq!(keychain.get("key"), Ok(Some("sk-test-1".into())));
        keychain.set("key", "sk-test-2").unwrap();
        assert_eq!(keychain.get("key"), Ok(Some("sk-test-2".into())));
        assert_eq!(keychain.names(), Ok(vec!["key".to_owned()]));
        keychain.delete("key").unwrap();
        assert_eq!(keychain.get("key"), Ok(None));
        assert_eq!(keychain.names(), Ok(Vec::new()));
        keychain.delete("key").unwrap();
    }

    #[cfg(target_os = "macos")]
    #[test]
    #[ignore = "writes to the login Keychain under a scratch service it deletes again: \
                cargo test -p letronna-core --release -- --ignored the_keychain"]
    fn the_keychain_keeps_any_text_a_secret_can_be() {
        let service = format!("Letronna-test-text-{}", std::process::id());
        let keychain = super::keychain::Keychain { service };
        let _tidy = Tidy(&keychain);
        for secret in [
            "deadbeef0011",
            "clé-ключ-🔑",
            "line one\nline two",
            r#"a "quoted" \ one"#,
            "it's $HOME # x\ttab",
        ] {
            keychain.set("key", secret).unwrap();
            assert_eq!(
                keychain.get("key"),
                Ok(Some(secret.to_owned())),
                "{secret:?}"
            );
        }
        // Refused whole rather than cut short where the tool's line ends.
        assert!(keychain.set("key", &"k".repeat(5000)).is_err());
        assert_eq!(keychain.get("key"), Ok(Some("it's $HOME # x\ttab".into())));
    }

    #[cfg(target_os = "macos")]
    #[test]
    #[ignore = "writes to the login Keychain under a scratch service it deletes again: \
                cargo test -p letronna-core --release -- --ignored the_keychain"]
    fn the_keychain_reads_what_an_older_build_saved_and_saves_it_anew_for_every_build() {
        use security_framework::os::macos::keychain::SecKeychain;
        use security_framework::passwords::{
            PasswordOptions, generic_password, set_generic_password,
        };
        let service = format!("Letronna-test-old-{}", std::process::id());
        // As a build before the tool saved it: straight from this program.
        set_generic_password(&service, "key", b"sk-old").unwrap();
        let keychain = super::keychain::Keychain { service };
        let _tidy = Tidy(&keychain);
        assert_eq!(keychain.get("key"), Ok(Some("sk-old".into())));
        keychain.set("key", "sk-new").unwrap();
        assert_eq!(keychain.get("key"), Ok(Some("sk-new".into())));
        // The tool's now, so this program is a stranger to it, as any other
        // build is, and only the tool reads it.
        let _quiet = SecKeychain::disable_user_interaction().unwrap();
        let options = PasswordOptions::new_generic_password(&keychain.service, "key");
        assert!(generic_password(options).is_err());
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn a_secret_the_keychain_tool_shows_reads_back_in_either_form() {
        use super::keychain::shown_as;
        assert_eq!(
            shown_as(r#""sk-abc_DEF-123""#),
            Ok(b"sk-abc_DEF-123".to_vec())
        );
        assert_eq!(shown_as(r#""deadbeef0011""#), Ok(b"deadbeef0011".to_vec()));
        let shown = r#"0x636CC3A9  "cl\303\251""#;
        assert_eq!(shown_as(shown), Ok("clé".as_bytes().to_vec()));
        let shown = r#"0x6C696E65310A6C696E6532  "line1\012line2""#;
        assert_eq!(shown_as(shown), Ok(b"line1\nline2".to_vec()));
        assert_eq!(shown_as(""), Ok(Vec::new()));
        assert!(shown_as("0x6G").is_err());
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn a_name_is_quoted_for_the_keychain_tool_and_a_line_break_refused() {
        use super::keychain::quoted;
        let quoted_name = quoted(r#"keys.My "odd" \ one"#);
        assert_eq!(quoted_name, Ok(r#""keys.My \"odd\" \\ one""#.to_owned()));
        assert!(quoted("keys.a\ndelete-keychain login.keychain").is_err());
    }
}
