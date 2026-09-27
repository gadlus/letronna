//! Letronna updating itself.
//!
//! A release is described by `latest.json`, and nothing in it is believed
//! until its Ed25519 signature checks out against a key compiled in here. The
//! signing happens on one person's machine, not in CI, so whoever gets hold of
//! the API, the GitHub account or the connection can at most withhold an
//! update -- never ship one. Every binary is then held to the SHA-256 and size
//! the signed description gives, and a version is only ever replaced by a
//! strictly newer one, so an old signed description cannot walk anyone back.
//!
//! A new binary is written beside the one it replaces, as `<name>.new`, so the
//! swap is a rename within one folder. Before that, it is run with `--version`
//! and has to name the version it was promised as: a broken download, the
//! wrong architecture or a missing library stops here, while the old binary is
//! still in place. The swap keeps the old binary as `<name>.old`, and a version
//! that is started twice without ever settling in (see [`confirm`]) is put
//! back on the third start and never offered again.
//!
//! Nothing here needs an administrator: only files Letronna's own install put
//! under the user's account are touched.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::auth;
use crate::chat::{self as backend};

/// This build's version, the one every release is compared with.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// The public halves of the keys releases are signed with, as the hex that
/// `openssl pkey -pubout -outform DER | tail -c 32` gives: the key in use, and
/// a backup kept offline so that losing the first does not strand every copy
/// already out there. Until one is set, every update is refused.
const RELEASE_KEYS: &[[u8; 32]] = &[];

/// A key whose private half is in the repository, `scripts/test-release-key.pem`,
/// so an update can be tried end to end. Only debug builds trust it.
const TEST_KEY: [u8; 32] = key("35b434c96b75f5bcef58d95bb5fbf62fea8d59b787d8ac452a3b68c59900d8fa");

/// Where the description of the newest release is, on the account API.
const MANIFEST: &str = "/app/releases/latest/latest.json";
const SIGNATURE: &str = "/app/releases/latest/latest.json.sig";

/// Far bigger than a description will ever be, and small enough that a
/// server answering with something else cannot fill memory.
const MAX_MANIFEST: u64 = 64 * 1024;
/// An Ed25519 signature is 64 bytes.
const MAX_SIGNATURE: u64 = 1024;
/// A binary bigger than this is refused whatever the description says; the
/// window is about 40 MB.
const MAX_BINARY: u64 = 256 * 1024 * 1024;

/// How many times a new version may start without settling in before the old
/// one is put back: one crash can be bad luck, two in a row is the release.
const GIVE_UP: u32 = 3;

/// How long a lock can be held before it is taken to be left behind by a
/// process that died. As long as the slowest download is allowed to take.
const STALE_LOCK: Duration = Duration::from_secs(30 * 60);

/// How long a new binary gets to say its version.
const SMOKE_TEST: Duration = Duration::from_secs(20);

/// A newer release than this build, and what it would replace here.
#[derive(Clone, Debug)]
pub struct Release {
    pub version: String,
    /// A page saying what changed.
    pub notes: String,
    /// Each binary, and the installed file it replaces.
    parts: Vec<(File, PathBuf)>,
    /// On macOS, the disk image to hand to the user instead: the app there
    /// is not replaced in place.
    package: Option<File>,
}

impl Release {
    /// Whether there is anything here this release can replace or hand over.
    pub fn installable(&self) -> bool {
        !self.parts.is_empty() || self.package.is_some()
    }
}

/// One file of a release, as the signed description gives it.
#[derive(Clone, Debug, Deserialize, Serialize)]
struct File {
    url: String,
    sha256: String,
    size: u64,
}

#[derive(Deserialize)]
struct Manifest {
    version: String,
    #[serde(default)]
    notes: String,
    platforms: HashMap<String, Platform>,
}

#[derive(Deserialize)]
struct Platform {
    window: Option<File>,
    terminal: Option<File>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Part {
    Window,
    Terminal,
}

/// What this computer knows about updates, in `update.json` beside the settings.
#[derive(Default, Serialize, Deserialize)]
struct State {
    /// When a check last finished, in seconds since the epoch.
    #[serde(default)]
    checked: u64,
    /// The newest version that check found.
    #[serde(default)]
    latest: String,
    /// Downloaded, checked, and waiting to be installed.
    #[serde(default)]
    staged: Option<Staged>,
    /// Installed, and not yet seen to start.
    #[serde(default)]
    pending: Option<Pending>,
    /// A version that was put back once; it is not offered again.
    #[serde(default)]
    skip: String,
}

#[derive(Serialize, Deserialize)]
struct Staged {
    version: String,
    files: Vec<StagedFile>,
}

#[derive(Serialize, Deserialize)]
struct StagedFile {
    dest: PathBuf,
    sha256: String,
}

#[derive(Serialize, Deserialize)]
struct Pending {
    from: String,
    to: String,
    starts: u32,
    files: Vec<PathBuf>,
}

impl State {
    fn path() -> PathBuf {
        backend::data_dir().join("update.json")
    }

    fn load() -> State {
        std::fs::read(Self::path())
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default()
    }

    fn save(&self) -> Result<(), String> {
        let path = Self::path();
        let _ = std::fs::create_dir_all(backend::data_dir());
        let bytes = serde_json::to_vec_pretty(self).map_err(|e| e.to_string())?;
        let temp = path.with_extension("tmp");
        std::fs::write(&temp, bytes)
            .and_then(|()| std::fs::rename(&temp, &path))
            .map_err(|e| format!("Could not save {}: {e}", path.display()))
    }
}

/// Whether this build updates itself. A development build runs where it was
/// built and must not replace the Letronna someone has installed, unless
/// `LETRONNA_UPDATES` is set to try the updater out.
pub fn enabled() -> bool {
    !cfg!(debug_assertions) || std::env::var_os("LETRONNA_UPDATES").is_some()
}

/// Ask for the newest release. `None` when this build is already the newest.
pub fn check() -> Result<Option<Release>, String> {
    if !enabled() {
        return Err("This build does not update itself.".into());
    }
    let Some(account) = auth::account() else {
        return Err("Sign in to get updates.".into());
    };
    let client = client()?;
    let token = Some(account.token.as_str());
    let manifest = fetch(&client, &auth::api(MANIFEST), token, MAX_MANIFEST)?;
    let signature = fetch(&client, &auth::api(SIGNATURE), token, MAX_SIGNATURE)?;
    let manifest = verify(&manifest, &signature, &trusted())?;
    let mut state = State::load();
    state.checked = backend::now();
    state.latest = manifest.version.clone();
    state.save()?;
    offer(manifest, VERSION, &state.skip, &destinations())
}

/// Download a release beside what it replaces, and check every file, so that
/// [`install`] has nothing left to go wrong but the renames.
pub fn download(release: &Release) -> Result<(), String> {
    if release.parts.is_empty() {
        return Err("Letronna is not installed here, so it cannot update itself.".into());
    }
    let _lock = Lock::take()?;
    let client = client()?;
    let account = auth::account();
    let mut files = Vec::new();
    for (file, dest) in &release.parts {
        // What the last update kept is only worth keeping until this one.
        for leftover in ["old", "failed"] {
            let _ = std::fs::remove_file(beside(dest, leftover));
        }
        let new = beside(dest, "new");
        let token = account
            .as_ref()
            .filter(|_| from_the_api(&file.url))
            .map(|a| a.token.as_str());
        let fetched = download_to(&client, file, token, &new).and_then(|()| {
            make_runnable(&new);
            says_version(&new, &release.version)
        });
        if let Err(error) = fetched {
            // Nothing is left beside Letronna from a release that failed.
            for (_, dest) in &release.parts {
                let _ = std::fs::remove_file(beside(dest, "new"));
            }
            return Err(error);
        }
        files.push(StagedFile {
            dest: dest.clone(),
            sha256: file.sha256.to_lowercase(),
        });
    }
    let mut state = State::load();
    state.staged = Some(Staged {
        version: release.version.clone(),
        files,
    });
    state.save()
}

/// On macOS, download the release's disk image into Downloads and return where
/// it is, for the user to open. The app there is not replaced in place.
pub fn save_package(release: &Release) -> Result<PathBuf, String> {
    let Some(file) = &release.package else {
        return Err("This release has nothing to download for this computer.".into());
    };
    let name = file
        .url
        .rsplit('/')
        .next()
        .filter(|name| !name.is_empty() && !name.contains(['\\', ':']))
        .unwrap_or("Letronna.dmg");
    let home = std::env::var_os("HOME").ok_or("No home folder to download into.")?;
    let path = PathBuf::from(home).join("Downloads").join(name);
    let client = client()?;
    let account = auth::account().filter(|_| from_the_api(&file.url));
    download_to(
        &client,
        file,
        account.as_ref().map(|a| a.token.as_str()),
        &path,
    )?;
    Ok(path)
}

/// The version downloaded and waiting to be installed, if any.
pub fn staged() -> Option<String> {
    State::load().staged.map(|staged| staged.version)
}

/// The newest version the last check found, when it is newer than this build.
pub fn seen() -> Option<String> {
    let state = State::load();
    (newer(&state.latest, VERSION) && state.latest != state.skip).then_some(state.latest)
}

/// Whether the last check was longer ago than `every`.
pub fn due(every: Duration) -> bool {
    backend::now().saturating_sub(State::load().checked) >= every.as_secs()
}

/// Put the downloaded release in place of the installed one. Returns this
/// program's own path when it was among what was replaced, for a window that
/// wants to start again as the new version.
pub fn install() -> Result<Option<PathBuf>, String> {
    let _lock = Lock::take()?;
    let mut state = State::load();
    let Some(staged) = state.staged.take() else {
        return Ok(None);
    };
    // The files sat on disk since they were checked; check them again.
    for file in &staged.files {
        let new = beside(&file.dest, "new");
        if sha256_file(&new).ok().as_deref() != Some(file.sha256.as_str()) {
            let _ = std::fs::remove_file(&new);
            state.save()?;
            return Err("The downloaded update changed on disk, so it was thrown away.".into());
        }
    }
    let dests: Vec<PathBuf> = staged.files.iter().map(|f| f.dest.clone()).collect();
    // Asked before the swap: on Linux the answer follows the running file
    // to its new name.
    let me = std::env::current_exe().ok();
    swap(&dests)?;
    state.pending = Some(Pending {
        from: VERSION.into(),
        to: staged.version,
        starts: 0,
        files: dests.clone(),
    });
    state.save()?;
    Ok(dests
        .into_iter()
        .find(|dest| me.as_deref().is_some_and(|me| same_file(me, dest))))
}

/// Called first thing when the window starts: install an update that is
/// waiting, or put back one that keeps failing to start. Returns the program
/// to start instead of carrying on, if there is one.
pub fn on_start() -> Option<PathBuf> {
    if !enabled() {
        return None;
    }
    let mut state = State::load();
    if state.pending.as_ref().is_some_and(|p| p.to == VERSION) {
        // Asked before anything moves, as in `install`.
        let me = std::env::current_exe().ok();
        let rolled_back = started(&mut state, VERSION);
        let _ = state.save();
        return me.filter(|_| rolled_back);
    }
    if state.staged.is_some() {
        return install().ok().flatten();
    }
    None
}

/// Say this version started and is working, so it is not put back.
pub fn confirm() {
    let mut state = State::load();
    let Some(pending) = state.pending.as_ref().filter(|p| p.to == VERSION) else {
        return;
    };
    for dest in &pending.files {
        let _ = std::fs::remove_file(beside(dest, "failed"));
    }
    state.pending = None;
    let _ = state.save();
}

/// Count a start of the version waiting to be confirmed, and put the old one
/// back when it has had its chances. True when it was put back.
fn started(state: &mut State, version: &str) -> bool {
    let Some(pending) = state.pending.as_mut().filter(|p| p.to == version) else {
        return false;
    };
    pending.starts += 1;
    if pending.starts < GIVE_UP {
        return false;
    }
    for dest in pending.files.iter().rev() {
        let old = beside(dest, "old");
        if !old.exists() {
            continue;
        }
        // The running program cannot be deleted on Windows, but it can be
        // moved aside.
        let failed = beside(dest, "failed");
        let _ = std::fs::remove_file(&failed);
        if rename(dest, &failed).is_ok() && rename(&old, dest).is_err() {
            let _ = rename(&failed, dest);
        }
    }
    state.skip = pending.to.clone();
    state.pending = None;
    true
}

/// Keep only what is newer than `current`, not refused before, and has a
/// file for each part of Letronna installed here.
fn offer(
    manifest: Manifest,
    current: &str,
    skip: &str,
    installed: &[(Part, PathBuf)],
) -> Result<Option<Release>, String> {
    if !newer(&manifest.version, current) || manifest.version == skip {
        return Ok(None);
    }
    let Some(platform) = manifest.platforms.get(&platform()) else {
        return Ok(None);
    };
    let mut parts = Vec::new();
    for (part, dest) in installed {
        let file = match part {
            Part::Window => &platform.window,
            Part::Terminal => &platform.terminal,
        };
        let Some(file) = file else {
            return Err(format!(
                "Letronna {} has no {} for this computer.",
                manifest.version,
                if *part == Part::Window {
                    "window"
                } else {
                    "terminal"
                }
            ));
        };
        parts.push((file.clone(), dest.clone()));
    }
    let package = cfg!(target_os = "macos")
        .then(|| platform.window.clone())
        .flatten();
    Ok(Some(Release {
        version: manifest.version,
        notes: manifest.notes,
        parts,
        package,
    }))
}

/// Whether `candidate` is a later version than `current`. Anything that does
/// not parse is never newer.
fn newer(candidate: &str, current: &str) -> bool {
    match (
        semver::Version::parse(candidate),
        semver::Version::parse(current),
    ) {
        (Ok(candidate), Ok(current)) => candidate > current,
        _ => false,
    }
}

/// Read the description, but only if one of `keys` signed exactly these bytes.
fn verify(bytes: &[u8], signature: &[u8], keys: &[[u8; 32]]) -> Result<Manifest, String> {
    use ring::signature::{ED25519, UnparsedPublicKey};
    let signed = keys.iter().any(|key| {
        UnparsedPublicKey::new(&ED25519, key)
            .verify(bytes, signature)
            .is_ok()
    });
    if !signed {
        return Err("The update is not signed by Letronna, so it was not installed.".into());
    }
    serde_json::from_slice(bytes).map_err(|_| "The update's description could not be read.".into())
}

fn trusted() -> Vec<[u8; 32]> {
    let mut keys = RELEASE_KEYS.to_vec();
    if cfg!(debug_assertions) {
        keys.push(TEST_KEY);
    }
    keys
}

/// What Letronna goes by on this computer: `windows-x64`, `linux-x64`, `macos-arm64`.
fn platform() -> String {
    let os = if cfg!(windows) {
        "windows"
    } else if cfg!(target_os = "macos") {
        "macos"
    } else {
        "linux"
    };
    let arch = if cfg!(target_arch = "aarch64") {
        "arm64"
    } else {
        "x64"
    };
    format!("{os}-{arch}")
}

/// Each part of Letronna installed here, at the places its installers put
/// them. Only files that exist count: whoever installed just the terminal
/// gets just the terminal.
fn destinations() -> Vec<(Part, PathBuf)> {
    let mut places = Vec::new();
    if cfg!(windows) {
        if let Some(local) = std::env::var_os("LOCALAPPDATA") {
            let root = PathBuf::from(local).join("Programs").join("Letronna");
            places.push((Part::Window, root.join("Letronna.exe")));
            places.push((Part::Terminal, root.join("bin").join("letronna.exe")));
        }
    } else if let Some(home) = std::env::var_os("HOME") {
        let home = PathBuf::from(home);
        let share = home.join(".local").join("share").join("letronna");
        // The window installs itself into ~/.local/bin on Linux; `letronna
        // setup` puts both parts under ~/.local/share. On macOS the window
        // is an .app the user put somewhere, and is not replaced.
        if cfg!(target_os = "linux") {
            places.push((
                Part::Window,
                home.join(".local").join("bin").join("letronna"),
            ));
            places.push((Part::Window, share.join("letronna-gui")));
        }
        places.push((Part::Terminal, share.join("bin").join("letronna")));
    }
    places.retain(|(_, path)| path.is_file());
    places
}

/// `Letronna.exe` becomes `Letronna.new.exe` and `letronna` becomes
/// `letronna.new`: the extension stays last, so Windows still sees a program.
fn beside(dest: &Path, tag: &str) -> PathBuf {
    let stem = dest
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let name = match dest.extension() {
        Some(ext) => format!("{stem}.{tag}.{}", ext.to_string_lossy()),
        None => format!("{stem}.{tag}"),
    };
    dest.with_file_name(name)
}

/// Move each `.new` into place, keeping what was there as `.old`. If any of
/// them fails, every one already moved is put back, so Letronna is never
/// left half one version and half another.
fn swap(dests: &[PathBuf]) -> Result<(), String> {
    let mut done: Vec<&PathBuf> = Vec::new();
    for dest in dests {
        if let Err(error) = swap_one(dest) {
            for dest in done.into_iter().rev() {
                if rename(dest, &beside(dest, "new")).is_ok() {
                    let _ = rename(&beside(dest, "old"), dest);
                }
            }
            return Err(error);
        }
        done.push(dest);
    }
    Ok(())
}

fn swap_one(dest: &Path) -> Result<(), String> {
    let old = beside(dest, "old");
    let new = beside(dest, "new");
    if !new.is_file() {
        return Err(format!("The update for {} is missing.", dest.display()));
    }
    if old.exists() && std::fs::remove_file(&old).is_err() {
        return Err(format!(
            "{} is still running from the last update. Close Letronna and try again.",
            old.display()
        ));
    }
    rename(dest, &old).map_err(|e| format!("Could not move {} aside: {e}", dest.display()))?;
    if let Err(e) = rename(&new, dest) {
        let _ = rename(&old, dest);
        return Err(format!(
            "Could not put the update in place of {}: {e}",
            dest.display()
        ));
    }
    Ok(())
}

/// A rename that tries a few times: right after a program is written, a
/// virus scanner often has it open for a moment.
fn rename(from: &Path, to: &Path) -> std::io::Result<()> {
    let mut tries = 0;
    loop {
        match std::fs::rename(from, to) {
            Ok(()) => return Ok(()),
            Err(error) if tries >= 5 || !from.exists() => return Err(error),
            Err(_) => {
                tries += 1;
                std::thread::sleep(Duration::from_millis(200));
            }
        }
    }
}

fn same_file(a: &Path, b: &Path) -> bool {
    match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}

/// Run the new binary with `--version` and hold it to the version it was
/// promised as. A program that does not answer in time is stopped.
fn says_version(program: &Path, version: &str) -> Result<(), String> {
    let mut command = std::process::Command::new(program);
    command
        .arg("--version")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    let mut child = command
        .spawn()
        .map_err(|e| format!("The update would not start: {e}"))?;
    let started = std::time::Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if started.elapsed() < SMOKE_TEST => {
                std::thread::sleep(Duration::from_millis(50))
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return Err("The update did not answer when asked its version.".into());
            }
        }
    }
    let mut said = String::new();
    if let Some(mut out) = child.stdout.take() {
        let _ = out.read_to_string(&mut said);
    }
    match said.split_whitespace().last() {
        Some(said) if said == version => Ok(()),
        Some(said) => Err(format!("The update says it is {said}, not {version}.")),
        None => Err("The update did not say its version.".into()),
    }
}

#[cfg(unix)]
fn make_runnable(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755));
}

#[cfg(not(unix))]
fn make_runnable(_: &Path) {}

fn client() -> Result<reqwest::blocking::Client, String> {
    reqwest::blocking::Client::builder()
        .connect_timeout(Duration::from_secs(10))
        // For the whole of a download, on a slow line.
        .timeout(STALE_LOCK)
        .user_agent(concat!("Letronna/", env!("CARGO_PKG_VERSION")))
        .redirect(reqwest::redirect::Policy::custom(|attempt| {
            if attempt.previous().len() >= 5 {
                attempt.error("too many redirects")
            } else if !allowed(attempt.url()) {
                attempt.error("an update only comes over https")
            } else {
                attempt.follow()
            }
        }))
        .build()
        .map_err(|e| e.to_string())
}

/// Updates come over https. Plain http is let through only to this computer,
/// where the tests serve them.
fn allowed(url: &reqwest::Url) -> bool {
    match url.scheme() {
        "https" => true,
        "http" => matches!(url.host_str(), Some("127.0.0.1" | "localhost" | "[::1]")),
        _ => false,
    }
}

/// Whether `url` is on the account API, the one server the sign-in may go to.
/// Once releases are public their files are elsewhere, and the token must
/// not follow them there.
fn from_the_api(url: &str) -> bool {
    let (Ok(url), Ok(api)) = (
        reqwest::Url::parse(url),
        reqwest::Url::parse(&auth::api("/")),
    ) else {
        return false;
    };
    url.scheme() == api.scheme()
        && url.host_str() == api.host_str()
        && url.port_or_known_default() == api.port_or_known_default()
}

/// A small response, whole, refused past `cap` bytes.
fn fetch(
    client: &reqwest::blocking::Client,
    url: &str,
    token: Option<&str>,
    cap: u64,
) -> Result<Vec<u8>, String> {
    let response = get(client, url, token)?;
    let mut bytes = Vec::new();
    response
        .take(cap + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| format!("The update check was cut off: {e}"))?;
    if bytes.len() as u64 > cap {
        return Err("The update server answered with something else.".into());
    }
    Ok(bytes)
}

fn get(
    client: &reqwest::blocking::Client,
    url: &str,
    token: Option<&str>,
) -> Result<reqwest::blocking::Response, String> {
    let parsed = reqwest::Url::parse(url).map_err(|_| format!("`{url}` is not a web address."))?;
    if !allowed(&parsed) {
        return Err("An update only comes over https.".into());
    }
    let mut request = client.get(parsed);
    // Sent only here: reqwest drops it when a redirect leaves this host.
    if let Some(token) = token {
        request = request.bearer_auth(token);
    }
    let response = request
        .send()
        .map_err(|e| format!("Could not reach the update server: {e}"))?;
    match response.status().as_u16() {
        200 => Ok(response),
        401 => Err("Sign in again to get updates.".into()),
        404 => Err("There is no release to update to yet.".into()),
        code => Err(format!("The update server answered {code}.")),
    }
}

/// Download `file` to `path`, hashing as it arrives, and keep it only if it
/// is exactly the size and SHA-256 promised.
fn download_to(
    client: &reqwest::blocking::Client,
    file: &File,
    token: Option<&str>,
    path: &Path,
) -> Result<(), String> {
    if file.size > MAX_BINARY {
        return Err("The update is larger than Letronna will download.".into());
    }
    let result = write_checked(client, file, token, path);
    if result.is_err() {
        let _ = std::fs::remove_file(path);
    }
    result
}

fn write_checked(
    client: &reqwest::blocking::Client,
    file: &File,
    token: Option<&str>,
    path: &Path,
) -> Result<(), String> {
    let mut response = get(client, &file.url, token)?;
    if response
        .content_length()
        .is_some_and(|len| len != file.size)
    {
        return Err("The update is not the size it should be.".into());
    }
    let mut out = std::fs::File::create(path)
        .map_err(|e| format!("Could not write {}: {e}", path.display()))?;
    let mut hash = ring::digest::Context::new(&ring::digest::SHA256);
    let mut total = 0u64;
    let mut block = vec![0; 64 * 1024];
    loop {
        let n = response
            .read(&mut block)
            .map_err(|e| format!("The download was cut off: {e}"))?;
        if n == 0 {
            break;
        }
        total += n as u64;
        if total > file.size {
            return Err("The update is larger than it should be.".into());
        }
        hash.update(&block[..n]);
        out.write_all(&block[..n])
            .map_err(|e| format!("Could not write {}: {e}", path.display()))?;
    }
    out.sync_all().map_err(|e| e.to_string())?;
    if total != file.size {
        return Err("The download was cut off.".into());
    }
    if hex(hash.finish().as_ref()) != file.sha256.to_lowercase() {
        return Err("The update does not match what was signed, so it was thrown away.".into());
    }
    Ok(())
}

fn sha256_file(path: &Path) -> std::io::Result<String> {
    let mut file = std::fs::File::open(path)?;
    let mut hash = ring::digest::Context::new(&ring::digest::SHA256);
    let mut block = vec![0; 64 * 1024];
    loop {
        let n = file.read(&mut block)?;
        if n == 0 {
            return Ok(hex(hash.finish().as_ref()));
        }
        hash.update(&block[..n]);
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// A public key written as 64 lowercase hex digits, read at compile time so a
/// mistyped one fails the build rather than every update.
const fn key(hex: &str) -> [u8; 32] {
    const fn digit(c: u8) -> u8 {
        match c {
            b'0'..=b'9' => c - b'0',
            b'a'..=b'f' => c - b'a' + 10,
            _ => panic!("a release key is 64 lowercase hex digits"),
        }
    }
    let hex = hex.as_bytes();
    assert!(hex.len() == 64, "a release key is 64 lowercase hex digits");
    let mut out = [0; 32];
    let mut i = 0;
    while i < 32 {
        out[i] = (digit(hex[2 * i]) << 4) | digit(hex[2 * i + 1]);
        i += 1;
    }
    out
}

/// Held while downloading or installing, so the window and the terminal do
/// not both write the same files at once.
struct Lock(PathBuf);

impl Lock {
    fn take() -> Result<Lock, String> {
        let _ = std::fs::create_dir_all(backend::data_dir());
        let path = backend::data_dir().join("update.lock");
        let stale = std::fs::metadata(&path)
            .and_then(|m| m.modified())
            .is_ok_and(|at| at.elapsed().unwrap_or_default() > STALE_LOCK);
        if stale {
            let _ = std::fs::remove_file(&path);
        }
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .map_err(|_| "Another Letronna is updating right now.".to_owned())?;
        Ok(Lock(path))
    }
}

impl Drop for Lock {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

#[cfg(test)]
mod tests {
    use super::{
        File, GIVE_UP, Manifest, Part, Pending, State, allowed, beside, download_to, fetch, hex,
        newer, offer, platform, started, swap, verify,
    };
    use ring::signature::{Ed25519KeyPair, KeyPair};
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::path::{Path, PathBuf};

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("letronna-update-{name}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn signer() -> (Ed25519KeyPair, [u8; 32]) {
        let pkcs8 = Ed25519KeyPair::generate_pkcs8(&ring::rand::SystemRandom::new()).unwrap();
        let pair = Ed25519KeyPair::from_pkcs8(pkcs8.as_ref()).unwrap();
        let public = pair.public_key().as_ref().try_into().unwrap();
        (pair, public)
    }

    fn manifest(version: &str) -> String {
        format!(
            r#"{{"version":"{version}","notes":"https://letronna.ai/changelog","platforms":{{"{}":{{
              "window":{{"url":"https://api.letronna.ai/app/releases/v{version}/w","sha256":"aa","size":1}},
              "terminal":{{"url":"https://api.letronna.ai/app/releases/v{version}/t","sha256":"bb","size":1}}}}}}}}"#,
            platform()
        )
    }

    fn parse(text: &str) -> Manifest {
        serde_json::from_str(text).unwrap()
    }

    fn sha256(bytes: &[u8]) -> String {
        hex(ring::digest::digest(&ring::digest::SHA256, bytes).as_ref())
    }

    /// Answer `count` requests on this computer, each with what `answer`
    /// makes of the request's head, and hand back the heads received.
    fn serve(
        count: usize,
        answer: impl Fn(&str) -> Vec<u8> + Send + 'static,
    ) -> (String, std::thread::JoinHandle<Vec<String>>) {
        let server = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", server.local_addr().unwrap());
        let worker = std::thread::spawn(move || {
            let mut heads = Vec::new();
            for _ in 0..count {
                let (mut socket, _) = server.accept().unwrap();
                let mut bytes = Vec::new();
                let mut block = [0; 4096];
                while !bytes.windows(4).any(|w| w == b"\r\n\r\n") {
                    let n = socket.read(&mut block).unwrap();
                    assert_ne!(n, 0);
                    bytes.extend_from_slice(&block[..n]);
                }
                let head = String::from_utf8_lossy(&bytes).to_lowercase();
                socket.write_all(&answer(&head)).unwrap();
                heads.push(head);
            }
            heads
        });
        (base, worker)
    }

    fn ok(body: &[u8]) -> Vec<u8> {
        let mut out = format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        )
        .into_bytes();
        out.extend_from_slice(body);
        out
    }

    fn client() -> reqwest::blocking::Client {
        super::client().unwrap()
    }

    #[test]
    fn a_manifest_signed_by_a_release_key_is_trusted() {
        let (pair, public) = signer();
        let text = manifest("9.0.0");
        let signature = pair.sign(text.as_bytes());
        let read = verify(text.as_bytes(), signature.as_ref(), &[[7; 32], public]).unwrap();
        assert_eq!(read.version, "9.0.0");
    }

    #[test]
    fn a_manifest_changed_after_signing_is_refused() {
        let (pair, public) = signer();
        let text = manifest("9.0.0");
        let signature = pair.sign(text.as_bytes());
        let changed = text.replace("\"sha256\":\"aa\"", "\"sha256\":\"ab\"");
        assert!(verify(changed.as_bytes(), signature.as_ref(), &[public]).is_err());
    }

    #[test]
    fn a_manifest_signed_by_another_key_is_refused() {
        let (pair, _) = signer();
        let (_, other) = signer();
        let text = manifest("9.0.0");
        let signature = pair.sign(text.as_bytes());
        assert!(verify(text.as_bytes(), signature.as_ref(), &[other]).is_err());
        assert!(verify(text.as_bytes(), signature.as_ref(), &[]).is_err());
        assert!(verify(text.as_bytes(), b"", &[other]).is_err());
    }

    #[test]
    fn the_test_key_is_the_one_in_the_repository() {
        let pem = include_str!("../../../scripts/test-release-key.pem");
        let body: String = pem.lines().filter(|l| !l.starts_with("-----")).collect();
        let der = base64(&body);
        // A PKCS#8 Ed25519 key is a fixed 16-byte header, then the seed.
        let pair = Ed25519KeyPair::from_seed_unchecked(&der[16..48]).unwrap();
        assert_eq!(pair.public_key().as_ref(), super::TEST_KEY);
    }

    fn base64(text: &str) -> Vec<u8> {
        let value = |c: u8| match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' => 62,
            _ => 63,
        };
        let digits: Vec<u8> = text.bytes().filter(|&c| c != b'=').map(value).collect();
        digits
            .chunks(4)
            .flat_map(|chunk| {
                let n = chunk.iter().fold(0u32, |n, &d| (n << 6) | d as u32);
                let n = n << (6 * (4 - chunk.len()));
                n.to_be_bytes()[1..chunk.len()].to_vec()
            })
            .collect()
    }

    #[test]
    fn an_older_or_equal_version_is_not_an_update() {
        assert!(newer("0.1.0-alpha.2", "0.1.0-alpha.1"));
        assert!(newer("0.1.0-alpha.10", "0.1.0-alpha.9"));
        assert!(newer("0.1.0-beta.1", "0.1.0-alpha.9"));
        assert!(newer("0.1.0", "0.1.0-beta.3"));
        assert!(newer("0.2.0", "0.1.9"));
        assert!(!newer("0.1.0-alpha.1", "0.1.0-alpha.1"));
        assert!(!newer("0.1.0-alpha.1", "0.1.0-alpha.2"));
        assert!(!newer("0.1.0-beta.3", "0.1.0"));
        assert!(!newer("", "0.1.0"));
        assert!(!newer("v0.2.0", "0.1.0"));
    }

    #[test]
    fn only_a_newer_release_for_this_computer_is_offered() {
        let installed = [
            (Part::Window, PathBuf::from("/w")),
            (Part::Terminal, PathBuf::from("/t")),
        ];
        assert!(
            offer(parse(&manifest("0.1.0")), "0.1.0", "", &installed)
                .unwrap()
                .is_none()
        );
        assert!(
            offer(parse(&manifest("0.2.0")), "0.1.0", "0.2.0", &installed)
                .unwrap()
                .is_none()
        );
        let release = offer(parse(&manifest("0.2.0")), "0.1.0", "", &installed)
            .unwrap()
            .unwrap();
        assert_eq!(release.version, "0.2.0");
        assert_eq!(release.parts.len(), 2);
        assert!(release.parts[0].0.url.ends_with("/w") && release.parts[0].1 == Path::new("/w"));
        let elsewhere = manifest("0.2.0").replace(&platform(), "plan9-x64");
        assert!(
            offer(parse(&elsewhere), "0.1.0", "", &installed)
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn plain_http_is_refused_except_on_this_computer() {
        let url = |s: &str| reqwest::Url::parse(s).unwrap();
        assert!(allowed(&url(
            "https://api.letronna.ai/app/releases/latest/latest.json"
        )));
        assert!(allowed(&url("http://127.0.0.1:8787/x")));
        assert!(allowed(&url("http://localhost:8787/x")));
        assert!(!allowed(&url("http://api.letronna.ai/x")));
        assert!(!allowed(&url("ftp://api.letronna.ai/x")));
        assert!(!allowed(&url("file:///C:/x")));
    }

    #[test]
    fn names_beside_a_program_keep_its_extension_last() {
        assert_eq!(
            beside(Path::new("/a/Letronna.exe"), "new"),
            Path::new("/a/Letronna.new.exe")
        );
        assert_eq!(
            beside(Path::new("/a/letronna"), "old"),
            Path::new("/a/letronna.old")
        );
        assert_eq!(
            beside(Path::new("/a/letronna-gui"), "new"),
            Path::new("/a/letronna-gui.new")
        );
    }

    #[test]
    fn a_download_that_matches_its_hash_is_kept() {
        let body = b"the new letronna".to_vec();
        let file_hash = sha256(&body);
        let (base, worker) = serve(1, move |_| ok(&body));
        let dir = scratch("download-ok");
        let path = dir.join("letronna.new");
        let file = File {
            url: format!("{base}/t"),
            sha256: file_hash,
            size: 16,
        };
        download_to(&client(), &file, None, &path).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"the new letronna");
        worker.join().unwrap();
    }

    #[test]
    fn a_download_that_does_not_match_its_hash_is_thrown_away() {
        let (base, worker) = serve(1, |_| ok(b"something else!!"));
        let dir = scratch("download-bad");
        let path = dir.join("letronna.new");
        let file = File {
            url: format!("{base}/t"),
            sha256: sha256(b"the new letronna"),
            size: 16,
        };
        let error = download_to(&client(), &file, None, &path).unwrap_err();
        assert!(error.contains("does not match"), "{error}");
        assert!(!path.exists());
        worker.join().unwrap();
    }

    #[test]
    fn a_download_larger_than_promised_stops() {
        // No length given up front, so only counting the bytes can catch it.
        let (base, worker) = serve(1, |_| {
            b"HTTP/1.1 200 OK\r\nConnection: close\r\n\r\nfar more than four bytes".to_vec()
        });
        let dir = scratch("download-long");
        let path = dir.join("letronna.new");
        let file = File {
            url: format!("{base}/t"),
            sha256: sha256(b"four"),
            size: 4,
        };
        assert!(download_to(&client(), &file, None, &path).is_err());
        assert!(!path.exists());
        worker.join().unwrap();
    }

    #[test]
    fn the_sign_in_is_not_sent_to_the_host_a_download_redirects_to() {
        let (files, files_seen) = serve(1, |_| ok(b"payload"));
        // Another port is another host, as far as the sign-in is concerned.
        let target = format!("{files}/letronna.exe");
        let (api, api_seen) = serve(1, move |_| {
            format!("HTTP/1.1 302 Found\r\nLocation: {target}\r\nContent-Length: 0\r\n\r\n")
                .into_bytes()
        });
        let bytes = fetch(
            &client(),
            &format!("{api}/app/releases/latest/letronna.exe"),
            Some("secret"),
            64,
        )
        .unwrap();
        assert_eq!(bytes, b"payload");
        assert!(api_seen.join().unwrap()[0].contains("authorization: bearer secret"));
        assert!(!files_seen.join().unwrap()[0].contains("authorization"));
    }

    fn installed(dir: &Path, name: &str, now: &str, new: Option<&str>) -> PathBuf {
        let dest = dir.join(name);
        std::fs::write(&dest, now).unwrap();
        if let Some(new) = new {
            std::fs::write(beside(&dest, "new"), new).unwrap();
        }
        dest
    }

    #[test]
    fn a_swap_keeps_the_old_binaries() {
        let dir = scratch("swap");
        let window = installed(&dir, "Letronna.exe", "window 1", Some("window 2"));
        let terminal = installed(&dir, "letronna", "terminal 1", Some("terminal 2"));
        swap(&[window.clone(), terminal.clone()]).unwrap();
        assert_eq!(std::fs::read_to_string(&window).unwrap(), "window 2");
        assert_eq!(std::fs::read_to_string(&terminal).unwrap(), "terminal 2");
        assert_eq!(
            std::fs::read_to_string(beside(&window, "old")).unwrap(),
            "window 1"
        );
        assert!(!beside(&terminal, "new").exists());
    }

    #[test]
    fn a_failed_swap_puts_every_binary_back() {
        let dir = scratch("swap-fails");
        let window = installed(&dir, "Letronna.exe", "window 1", Some("window 2"));
        // The terminal's download is missing, so the second swap fails.
        let terminal = installed(&dir, "letronna", "terminal 1", None);
        assert!(swap(&[window.clone(), terminal.clone()]).is_err());
        assert_eq!(std::fs::read_to_string(&window).unwrap(), "window 1");
        assert_eq!(std::fs::read_to_string(&terminal).unwrap(), "terminal 1");
        assert_eq!(
            std::fs::read_to_string(beside(&window, "new")).unwrap(),
            "window 2"
        );
        assert!(!beside(&window, "old").exists());
    }

    #[test]
    fn a_version_that_keeps_failing_to_start_is_rolled_back() {
        let dir = scratch("roll-back");
        let window = installed(&dir, "Letronna.exe", "window 1", Some("window 2"));
        swap(std::slice::from_ref(&window)).unwrap();
        let mut state = State {
            pending: Some(Pending {
                from: "1.0.0".into(),
                to: "2.0.0".into(),
                starts: 0,
                files: vec![window.clone()],
            }),
            ..Default::default()
        };
        assert!(
            !started(&mut state, "1.0.0"),
            "another version's start is not counted"
        );
        for _ in 1..GIVE_UP {
            assert!(!started(&mut state, "2.0.0"));
            assert_eq!(std::fs::read_to_string(&window).unwrap(), "window 2");
        }
        assert!(started(&mut state, "2.0.0"));
        assert_eq!(std::fs::read_to_string(&window).unwrap(), "window 1");
        assert_eq!(
            std::fs::read_to_string(beside(&window, "failed")).unwrap(),
            "window 2"
        );
        assert_eq!(state.skip, "2.0.0");
        assert!(state.pending.is_none());
    }

    #[test]
    #[ignore = "needs a signed-in account and a published release: cargo test -p letronna-core -- --ignored live_manifest"]
    fn the_live_manifest_verifies() {
        let release = super::check().unwrap();
        println!("{:?}", release.map(|r| r.version));
    }
}
