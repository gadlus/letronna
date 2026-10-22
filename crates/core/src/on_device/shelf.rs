//! The files of models on this computer: downloading them, checking them and
//! deleting them, and the llama.cpp build that runs them.
//!
//! A model is gigabytes, so a download survives everything short of the
//! file changing: it goes to `<file>.part`, with `<file>.part.json` naming
//! what it is, and carries on from there after a pause, a dropped
//! connection or a quit, by asking for the rest with a Range. Hugging
//! Face's resolve address answers with a redirect to a signed copy that
//! lasts an hour, so every attempt asks it again rather than keeping the
//! copy's address. A file is moved into place only once its size and
//! SHA-256 are what the catalog says, so nothing half-arrived or altered is
//! ever run. One download runs at a time; the rest wait their turn.
//!
//! The front ends watch `listen()` and read `state()`; nothing here draws.

use super::catalog::{self, BUILD, Entry, Pack, Runtime};
use super::fit::{self, size};
use super::machine::{self, Backend, Device, Machine};
use async_channel::{Receiver, Sender};
use std::collections::{HashMap, HashSet, VecDeque};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError};
use std::time::{Duration, Instant};

/// Room kept free beyond a download, so the system and the apps writing
/// beside it are never the ones to run out.
const DISK_MARGIN: u64 = 2_000_000_000;
/// Tries at a dropped connection before a download pauses.
const TRIES: u32 = 3;
/// A connection that sends nothing for this long is dropped and tried again.
const STALL: Duration = Duration::from_secs(60);
/// How often a download says how far it has got: a few times a second is
/// all a progress ring can show.
const TELL_EVERY: Duration = Duration::from_millis(250);
/// The first wait before trying a dropped connection again; it doubles.
#[cfg(not(test))]
const BACKOFF: Duration = Duration::from_secs(1);
#[cfg(test)]
const BACKOFF: Duration = Duration::from_millis(5);
/// How long a new runtime gets to say its version and list its devices.
const SMOKE_TEST: Duration = Duration::from_secs(30);

/// Said when a file is not at its address any more.
pub const MOVED: &str = "The file has moved. Update Letronna to get it.";
/// Said when a finished download is not what the catalog promised.
pub const DAMAGED: &str = "The download was damaged, so it was thrown away. Try again.";
/// Said when a download cannot reach the internet.
pub const NO_INTERNET: &str = "No internet. Connect, then resume; after that it works offline.";
/// Said when the disk fills part way.
pub const DISK_FULL: &str = "The disk is full. Free some space, then resume.";

/// Where models are kept.
pub fn dir() -> PathBuf {
    crate::chat::base_dir().join("models")
}

/// Where llama.cpp builds are kept.
pub fn runtimes() -> PathBuf {
    crate::chat::base_dir().join("runtime")
}

/// Where a file of `entry` lives once here.
pub fn path(entry: &Entry, pack: &Pack) -> PathBuf {
    dir().join(entry.id).join(pack.file)
}

/// How far one file has got.
#[derive(Debug, Clone, PartialEq)]
pub enum State {
    /// Not here.
    None,
    /// Waiting for the download before it.
    Queued,
    Getting {
        got: u64,
        total: u64,
    },
    /// A part kept from before, read again to carry on its hash.
    Checking {
        got: u64,
        total: u64,
    },
    /// Stopped part way: by the user, a quit, the connection or the disk.
    Paused {
        got: u64,
        total: u64,
        why: Option<String>,
    },
    Ready,
    Failed(String),
}

impl State {
    /// How much of it is here, from 0 to 1, while it is on its way.
    pub fn share(&self) -> Option<f32> {
        match self {
            State::Getting { got, total }
            | State::Checking { got, total }
            | State::Paused { got, total, .. } => Some(*got as f32 / (*total).max(1) as f32),
            State::Queued => Some(0.0),
            _ => None,
        }
    }

    /// Whether it is on its way or waiting to be.
    pub fn moving(&self) -> bool {
        matches!(
            self,
            State::Queued | State::Getting { .. } | State::Checking { .. }
        )
    }

    /// What it reads as beside a model: "Paused · 1.4 of 5.6 GB".
    pub fn label(&self) -> Option<String> {
        let of = |got: u64, total: u64| format!("{} of {}", size(got), size(total));
        match self {
            State::Queued => Some("Waiting".into()),
            State::Getting { got, total } => Some(of(*got, *total)),
            State::Checking { .. } => Some("Checking".into()),
            State::Paused {
                got,
                total,
                why: None,
            } => Some(format!("Paused · {}", of(*got, *total))),
            State::Paused { why: Some(why), .. } => Some(why.clone()),
            State::Failed(why) => Some(why.clone()),
            State::None | State::Ready => None,
        }
    }
}

/// What is on the queue: a model's file, or a llama.cpp build.
#[derive(Clone, Copy)]
enum What {
    Pack(&'static Entry, Pack),
    Runtime(&'static Runtime),
}

struct Shelf {
    /// Files on their way, paused or failed, by key; anything else is read
    /// off the disk.
    states: HashMap<String, State>,
    queue: VecDeque<(String, What)>,
    /// The download running, and the flag that stops it.
    running: Option<(String, Arc<AtomicBool>)>,
    /// Downloads stopped to be thrown away rather than paused.
    cancelled: HashSet<String>,
    scanned: bool,
}

fn shelf() -> MutexGuard<'static, Shelf> {
    static SHELF: OnceLock<Mutex<Shelf>> = OnceLock::new();
    let mut shelf = SHELF
        .get_or_init(|| {
            Mutex::new(Shelf {
                states: HashMap::new(),
                queue: VecDeque::new(),
                running: None,
                cancelled: HashSet::new(),
                scanned: false,
            })
        })
        .lock()
        // Nothing that can panic runs under the lock.
        .unwrap_or_else(PoisonError::into_inner);
    if !shelf.scanned {
        shelf.scanned = true;
        shelf.states.extend(left_from_before());
    }
    shelf
}

/// Downloads a quit left part way, as paused, so they can be resumed.
fn left_from_before() -> Vec<(String, State)> {
    let mut found = Vec::new();
    for entry in catalog::CATALOG {
        for pack in packs(entry) {
            let part = beside(&path(entry, pack), "part");
            if let Ok(meta) = std::fs::metadata(&part)
                && note_matches(&beside(&path(entry, pack), "part.json"), pack)
            {
                found.push((
                    key(entry, pack),
                    State::Paused {
                        got: meta.len().min(pack.size),
                        total: pack.size,
                        why: None,
                    },
                ));
            }
        }
    }
    found
}

fn packs(entry: &Entry) -> impl Iterator<Item = &Pack> {
    [
        Some(&entry.file),
        entry.smaller.as_ref(),
        entry.eyes.as_ref(),
    ]
    .into_iter()
    .flatten()
}

fn key(entry: &Entry, pack: &Pack) -> String {
    format!("{}/{}", entry.id, pack.file)
}

fn runtime_key(runtime: &Runtime) -> String {
    format!("runtime/{}", runtime.backend.name())
}

/// The doorbell holds a receiver of its own, so a ring with nobody listening
/// is kept rather than refused; rings while nobody answers merge into one.
fn doorbell() -> &'static (Sender<()>, Receiver<()>) {
    static DOORBELL: OnceLock<(Sender<()>, Receiver<()>)> = OnceLock::new();
    DOORBELL.get_or_init(|| async_channel::bounded(1))
}

fn ring() {
    let _ = doorbell().0.try_send(());
}

/// The doorbell a front end waits on to redraw, rung whenever a download
/// moves, stops, ends or a file is deleted.
pub fn listen() -> Receiver<()> {
    doorbell().1.clone()
}

fn set(key: &str, state: State) {
    shelf().states.insert(key.to_owned(), state);
    ring();
}

/// How far `pack` of `entry` has got.
pub fn state(entry: &Entry, pack: &Pack) -> State {
    if let Some(state) = shelf().states.get(&key(entry, pack)) {
        return state.clone();
    }
    if on_disk(&path(entry, pack), pack.size) {
        State::Ready
    } else {
        State::None
    }
}

fn on_disk(path: &Path, size: u64) -> bool {
    std::fs::metadata(path).is_ok_and(|m| m.is_file() && m.len() == size)
}

/// The model file of `entry` that is here, the main one or the smaller.
pub fn ready(entry: &Entry) -> Option<&Pack> {
    [Some(&entry.file), entry.smaller.as_ref()]
        .into_iter()
        .flatten()
        .find(|pack| state(entry, pack) == State::Ready)
}

/// The ids of the models that are here, in the catalog's order.
pub fn ready_ids() -> Vec<&'static str> {
    catalog::CATALOG
        .iter()
        .filter(|entry| ready(entry).is_some())
        .map(|entry| entry.id)
        .collect()
}

/// Whether `entry`'s vision pack is here.
pub fn eyes_ready(entry: &Entry) -> bool {
    entry
        .eyes
        .as_ref()
        .is_some_and(|eyes| state(entry, eyes) == State::Ready)
}

/// What the models here take on the disk, parts included.
pub fn used() -> u64 {
    fn walk(dir: &Path) -> u64 {
        let Ok(items) = std::fs::read_dir(dir) else {
            return 0;
        };
        items
            .flatten()
            .map(|item| match item.metadata() {
                Ok(meta) if meta.is_dir() => walk(&item.path()),
                Ok(meta) => meta.len(),
                Err(_) => 0,
            })
            .sum()
    }
    walk(&dir()) + walk(&runtimes())
}

/// What `entry`'s files take on the disk, parts included.
pub fn used_by(entry: &Entry) -> u64 {
    let Ok(items) = std::fs::read_dir(dir().join(entry.id)) else {
        return 0;
    };
    items
        .flatten()
        .filter_map(|item| item.metadata().ok())
        .map(|meta| meta.len())
        .sum()
}

/// The free space on the disk the models go to.
pub fn free_space() -> Option<u64> {
    let dir = dir();
    let _ = std::fs::create_dir_all(&dir);
    let dir = std::fs::canonicalize(&dir).unwrap_or(dir);
    let disks = sysinfo::Disks::new_with_refreshed_list();
    disks
        .list()
        .iter()
        .filter(|disk| dir.starts_with(disk.mount_point()))
        .max_by_key(|disk| disk.mount_point().as_os_str().len())
        .map(|disk| disk.available_space())
}

/// Download `entry` to run on `machine`, and the llama.cpp build it needs
/// if that is not here yet. Refused at once when it cannot run here or the
/// disk has no room; anything later shows in its state.
pub fn get(entry: &'static Entry, machine: &Machine) -> Result<(), String> {
    let fit = fit::fit(entry, machine, false);
    if !fit.verdict.runs() {
        return Err(fit.verdict.why().unwrap_or_else(|| fit.verdict.label()));
    }
    let pack = *fit.pack(entry);
    let runtime = catalog::runtime(&machine.os, &machine.arch, fit.choice.backend)
        .filter(|runtime| installed(runtime.backend).is_none());
    let mut jobs = Vec::new();
    if let Some(runtime) = runtime {
        jobs.push((runtime_key(runtime), What::Runtime(runtime)));
    }
    jobs.push((key(entry, &pack), What::Pack(entry, pack)));
    queue(jobs)
}

/// Download `entry`'s vision pack, so it can read photos.
pub fn get_eyes(entry: &'static Entry) -> Result<(), String> {
    let Some(eyes) = entry.eyes else {
        return Err(format!(
            "{} can't read photos. Pick a model that can.",
            entry.name
        ));
    };
    queue(vec![(key(entry, &eyes), What::Pack(entry, eyes))])
}

/// Download the llama.cpp build for `backend` on this computer.
pub fn get_runtime(machine: &Machine, backend: Backend) -> Result<(), String> {
    let Some(runtime) = catalog::runtime(&machine.os, &machine.arch, backend) else {
        return Err("Letronna has no llama.cpp build for this kind of computer yet.".into());
    };
    queue(vec![(runtime_key(runtime), What::Runtime(runtime))])
}

fn queue(jobs: Vec<(String, What)>) -> Result<(), String> {
    let needed: u64 = jobs.iter().map(|(_, what)| remaining(what)).sum();
    if let Some(free) = free_space()
        && free < needed + DISK_MARGIN
    {
        return Err(format!(
            "Not enough space: it needs {} free and the disk has {}.",
            size(needed + DISK_MARGIN),
            size(free)
        ));
    }
    let mut shelf = shelf();
    for (key, what) in jobs {
        let busy = shelf.queue.iter().any(|(k, _)| *k == key)
            || shelf.running.as_ref().is_some_and(|(k, _)| *k == key);
        if busy {
            continue;
        }
        shelf.cancelled.remove(&key);
        shelf.states.insert(key.clone(), State::Queued);
        shelf.queue.push_back((key, what));
    }
    if shelf.running.is_none() {
        shelf.running = Some((String::new(), Arc::new(AtomicBool::new(false))));
        std::thread::spawn(work);
    }
    drop(shelf);
    ring();
    Ok(())
}

/// Bytes still to come for `what`.
fn remaining(what: &What) -> u64 {
    match what {
        What::Pack(entry, pack) => {
            let part = beside(&path(entry, pack), "part");
            pack.size
                - std::fs::metadata(part)
                    .map_or(0, |m| m.len())
                    .min(pack.size)
        }
        // Each archive is kept until it is unpacked, beside what it unpacks to.
        What::Runtime(runtime) => [Some(runtime.archive), runtime.extra]
            .into_iter()
            .flatten()
            .map(|asset| asset.size * 3)
            .sum(),
    }
}

/// Stop a download, keeping what has arrived to resume from.
pub fn pause(entry: &Entry, pack: &Pack) {
    stop(&key(entry, pack), false);
}

/// Carry on a paused download.
pub fn resume(entry: &'static Entry, pack: &Pack) -> Result<(), String> {
    queue(vec![(key(entry, pack), What::Pack(entry, *pack))])
}

/// Stop a download and throw away what has arrived.
pub fn cancel(entry: &Entry, pack: &Pack) {
    stop(&key(entry, pack), true);
    let model = path(entry, pack);
    let _ = std::fs::remove_file(beside(&model, "part"));
    let _ = std::fs::remove_file(beside(&model, "part.json"));
}

fn stop(key: &str, cancel: bool) {
    let mut shelf = shelf();
    if cancel {
        shelf.cancelled.insert(key.to_owned());
    }
    shelf.queue.retain(|(k, _)| k != key);
    if let Some((running, flag)) = &shelf.running
        && running == key
    {
        flag.store(true, Ordering::SeqCst);
    }
    let paused = match shelf.states.get(key) {
        _ if cancel => State::None,
        Some(State::Getting { got, total } | State::Checking { got, total }) => State::Paused {
            got: *got,
            total: *total,
            why: None,
        },
        Some(State::Queued) | None => State::None,
        Some(other) => other.clone(),
    };
    shelf.states.insert(key.to_owned(), paused);
    drop(shelf);
    ring();
}

/// Delete `entry` from this computer, its vision pack and any part with
/// it; the server running it is stopped first, as Windows cannot delete a
/// file a process has mapped.
pub fn delete(entry: &Entry) -> Result<(), String> {
    for pack in packs(entry) {
        stop(&key(entry, pack), true);
    }
    super::runtime::let_go(entry.id);
    let folder = dir().join(entry.id);
    let result = match std::fs::remove_dir_all(&folder) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(format!("Could not delete {}: {e}", entry.name)),
    };
    let mut shelf = shelf();
    for pack in packs(entry) {
        shelf.states.remove(&key(entry, pack));
    }
    drop(shelf);
    ring();
    result
}

/// Delete `entry`'s vision pack alone.
pub fn delete_eyes(entry: &Entry) -> Result<(), String> {
    let Some(eyes) = entry.eyes else {
        return Ok(());
    };
    stop(&key(entry, &eyes), true);
    if super::runtime::serving_photos(entry.id) {
        super::runtime::let_go(entry.id);
    }
    let file = path(entry, &eyes);
    let _ = std::fs::remove_file(beside(&file, "part"));
    let _ = std::fs::remove_file(beside(&file, "part.json"));
    let result = match std::fs::remove_file(&file) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(format!("Could not delete photo reading: {e}")),
    };
    shelf().states.remove(&key(entry, &eyes));
    ring();
    result
}

/// The download thread: one file at a time, until the queue is empty.
fn work() {
    loop {
        let (key, what, flag) = {
            let mut shelf = shelf();
            let Some((key, what)) = shelf.queue.pop_front() else {
                shelf.running = None;
                return;
            };
            let flag = Arc::new(AtomicBool::new(false));
            shelf.running = Some((key.clone(), flag.clone()));
            (key, what, flag)
        };
        let result = match what {
            What::Pack(entry, pack) => fetch(
                &pack.url(),
                pack.size,
                pack.sha256,
                &path(entry, &pack),
                &key,
                &flag,
            ),
            What::Runtime(runtime) => install(runtime, &key, &flag),
        };
        let cancelled = shelf().cancelled.remove(&key);
        let state = match result {
            _ if cancelled => {
                if let What::Pack(entry, pack) = what {
                    let model = path(entry, &pack);
                    let _ = std::fs::remove_file(beside(&model, "part"));
                    let _ = std::fs::remove_file(beside(&model, "part.json"));
                }
                State::None
            }
            Ok(()) => State::Ready,
            Err(Stop::Paused { got, total, why }) => State::Paused { got, total, why },
            Err(Stop::Failed(why)) => State::Failed(why),
        };
        let ready = state == State::Ready;
        set(&key, state);
        if ready {
            shelf().states.remove(&key);
            ring();
        }
    }
}

/// Why a download stopped short.
#[derive(Debug, PartialEq)]
enum Stop {
    Paused {
        got: u64,
        total: u64,
        why: Option<String>,
    },
    Failed(String),
}

/// What `<file>.part.json` holds: enough to know the part is of this file.
#[derive(serde::Serialize, serde::Deserialize, PartialEq)]
struct Note {
    url: String,
    size: u64,
    sha256: String,
}

fn note_matches(note: &Path, pack: &Pack) -> bool {
    std::fs::read(note)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<Note>(&bytes).ok())
        .is_some_and(|note| note.size == pack.size && note.sha256 == pack.sha256)
}

/// `model.gguf` with `.part` after it.
fn beside(path: &Path, tag: &str) -> PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(".");
    name.push(tag);
    path.with_file_name(name)
}

/// Download `url` to `dest`, carrying on from a part left before, and move
/// it into place only if it is `size` bytes with this SHA-256.
fn fetch(
    url: &str,
    size: u64,
    sha256: &str,
    dest: &Path,
    key: &str,
    stop: &AtomicBool,
) -> Result<(), Stop> {
    let failed = |why: String| Stop::Failed(why);
    let part = beside(dest, "part");
    let note_path = beside(dest, "part.json");
    if let Some(folder) = dest.parent() {
        std::fs::create_dir_all(folder)
            .map_err(|e| failed(format!("Could not make {}: {e}", folder.display())))?;
    }
    let note = Note {
        url: url.to_owned(),
        size,
        sha256: sha256.to_owned(),
    };
    let mut hash = ring::digest::Context::new(&ring::digest::SHA256);
    let mut got = 0;
    let kept = std::fs::read(&note_path)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<Note>(&bytes).ok())
        .is_some_and(|before| before.size == size && before.sha256 == sha256);
    if kept && let Ok(meta) = std::fs::metadata(&part) {
        let len = meta.len();
        if len <= size {
            set(
                key,
                State::Checking {
                    got: len,
                    total: size,
                },
            );
            got = rehash(&part, len, &mut hash, stop).map_err(|e| match e {
                None => Stop::Paused {
                    got: len,
                    total: size,
                    why: None,
                },
                Some(e) => failed(format!("Could not read {}: {e}", part.display())),
            })?;
        }
    }
    // Opened to write and moved to its end rather than opened to append: on
    // Windows an append handle may neither be cut back, as `set_len` below
    // and a server that ignores the range do, nor flushed to disk.
    let mut out = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(&part)
        .map_err(|e| failed(format!("Could not write {}: {e}", part.display())))?;
    out.seek(SeekFrom::Start(got))
        .map_err(|e| failed(format!("Could not write {}: {e}", part.display())))?;
    if got == 0 {
        out.set_len(0).map_err(|e| failed(e.to_string()))?;
        let text = serde_json::to_vec(&note).map_err(|e| failed(e.to_string()))?;
        std::fs::write(&note_path, text).map_err(|e| failed(e.to_string()))?;
    }
    let client = client().map_err(failed)?;
    let mut tries = 0;
    let mut told = Instant::now();
    while got < size {
        let paused = |got: u64, why: Option<&str>| Stop::Paused {
            got,
            total: size,
            why: why.map(str::to_owned),
        };
        if stop.load(Ordering::SeqCst) {
            return Err(paused(got, None));
        }
        let before = got;
        match transfer(
            &client, url, size, &mut got, &mut hash, &mut out, key, stop, &mut told,
        ) {
            Ok(()) => {}
            Err(Attempt::Stopped) => return Err(paused(got, None)),
            Err(Attempt::Moved) => {
                let _ = std::fs::remove_file(&part);
                let _ = std::fs::remove_file(&note_path);
                return Err(failed(MOVED.into()));
            }
            Err(Attempt::Disk(e)) if e.kind() == std::io::ErrorKind::StorageFull => {
                return Err(paused(got, Some(DISK_FULL)));
            }
            Err(Attempt::Disk(e)) => {
                return Err(failed(format!("Could not write {}: {e}", part.display())));
            }
            Err(Attempt::Network) => {
                tries = if got > before { 1 } else { tries + 1 };
                if tries >= TRIES {
                    if got == 0 {
                        let _ = std::fs::remove_file(&part);
                        let _ = std::fs::remove_file(&note_path);
                        return Err(failed(NO_INTERNET.into()));
                    }
                    return Err(paused(got, Some(NO_INTERNET)));
                }
                let wait = BACKOFF * 2u32.pow(tries);
                let until = Instant::now() + wait;
                while Instant::now() < until {
                    if stop.load(Ordering::SeqCst) {
                        return Err(paused(got, None));
                    }
                    std::thread::sleep(Duration::from_millis(100));
                }
            }
        }
    }
    out.sync_all().map_err(|e| failed(e.to_string()))?;
    drop(out);
    set(key, State::Checking { got, total: size });
    if hex(hash.finish().as_ref()) != sha256.to_ascii_lowercase() {
        let _ = std::fs::remove_file(&part);
        let _ = std::fs::remove_file(&note_path);
        return Err(failed(DAMAGED.into()));
    }
    std::fs::rename(&part, dest).map_err(|e| failed(format!("Could not keep it: {e}")))?;
    let _ = std::fs::remove_file(&note_path);
    keep_out_of_backups(&dir());
    Ok(())
}

/// Read a kept part into `hash`; `Err(None)` when stopped on the way.
fn rehash(
    part: &Path,
    len: u64,
    hash: &mut ring::digest::Context,
    stop: &AtomicBool,
) -> Result<u64, Option<std::io::Error>> {
    let mut file = std::fs::File::open(part).map_err(Some)?;
    let mut block = vec![0; 1 << 20];
    let mut read = 0;
    while read < len {
        if stop.load(Ordering::SeqCst) {
            return Err(None);
        }
        let n = file.read(&mut block).map_err(Some)?;
        if n == 0 {
            break;
        }
        let n = n.min((len - read) as usize);
        hash.update(&block[..n]);
        read += n as u64;
    }
    Ok(read)
}

/// How one request for the file ended, short of the whole of it.
enum Attempt {
    Stopped,
    Moved,
    Network,
    Disk(std::io::Error),
}

/// Ask for the rest of the file once and write what comes.
#[allow(clippy::too_many_arguments)]
fn transfer(
    client: &reqwest::blocking::Client,
    url: &str,
    size: u64,
    got: &mut u64,
    hash: &mut ring::digest::Context,
    out: &mut std::fs::File,
    key: &str,
    stop: &AtomicBool,
    told: &mut Instant,
) -> Result<(), Attempt> {
    let mut request = client.get(url);
    if *got > 0 {
        request = request.header(reqwest::header::RANGE, format!("bytes={got}-"));
    }
    let mut response = request.send().map_err(|_| Attempt::Network)?;
    let status = response.status().as_u16();
    match status {
        206 => {
            let range = response
                .headers()
                .get(reqwest::header::CONTENT_RANGE)
                .and_then(|v| v.to_str().ok())
                .unwrap_or_default();
            let expected = format!("bytes {got}-{}/{size}", size - 1);
            if range != expected {
                // A part of something else: start over.
                restart(got, hash, out)?;
                return Ok(());
            }
        }
        200 => {
            // The whole file: the server ignored the Range.
            if *got > 0 {
                restart(got, hash, out)?;
            }
            if response.content_length().is_some_and(|len| len != size) {
                return Err(Attempt::Moved);
            }
        }
        416 if *got == size => return Ok(()),
        408 | 425 | 429 | 500..=599 => return Err(Attempt::Network),
        _ => return Err(Attempt::Moved),
    }
    let html = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|kind| kind.starts_with("text/html"));
    if html {
        return Err(Attempt::Moved);
    }
    let mut block = vec![0; 1 << 20];
    loop {
        if stop.load(Ordering::SeqCst) {
            return Err(Attempt::Stopped);
        }
        let n = match response.read(&mut block) {
            Ok(0) => return Ok(()),
            Ok(n) => n,
            Err(_) => return Err(Attempt::Network),
        };
        if *got + n as u64 > size {
            return Err(Attempt::Moved);
        }
        out.write_all(&block[..n]).map_err(Attempt::Disk)?;
        hash.update(&block[..n]);
        *got += n as u64;
        if told.elapsed() >= TELL_EVERY {
            *told = Instant::now();
            set(
                key,
                State::Getting {
                    got: *got,
                    total: size,
                },
            );
        }
    }
}

fn restart(
    got: &mut u64,
    hash: &mut ring::digest::Context,
    out: &mut std::fs::File,
) -> Result<(), Attempt> {
    out.set_len(0).map_err(Attempt::Disk)?;
    out.seek(SeekFrom::Start(0)).map_err(Attempt::Disk)?;
    *got = 0;
    *hash = ring::digest::Context::new(&ring::digest::SHA256);
    Ok(())
}

/// The downloads' own client: redirects followed only over https, as the
/// updater's are, and a connection dropped once it sends nothing for a
/// minute -- reqwest's blocking timeout is per read, not for the whole file.
fn client() -> Result<reqwest::blocking::Client, String> {
    reqwest::blocking::Client::builder()
        .connect_timeout(Duration::from_secs(15))
        .timeout(STALL)
        .user_agent(concat!("Letronna/", env!("CARGO_PKG_VERSION")))
        .redirect(reqwest::redirect::Policy::custom(|attempt| {
            if attempt.previous().len() >= 5 {
                attempt.error("too many redirects")
            } else if !crate::update::allowed(attempt.url()) {
                attempt.error("a model only comes over https")
            } else {
                attempt.follow()
            }
        }))
        .build()
        .map_err(|e| e.to_string())
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Keep `dir` out of Time Machine: a model is gigabytes anyone can download
/// again. The exclusion is on the folder, so files moved in keep it.
#[cfg(target_os = "macos")]
fn keep_out_of_backups(dir: &Path) {
    let _ = std::process::Command::new("/usr/bin/tmutil")
        .arg("addexclusion")
        .arg(dir)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();
}

#[cfg(not(target_os = "macos"))]
fn keep_out_of_backups(_: &Path) {}

/// A llama.cpp build that is here and has answered.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Installed {
    pub backend: Backend,
    pub build: u32,
    /// `llama-server`, where the archive put it.
    pub server: PathBuf,
    /// What `--list-devices` said when it was installed.
    pub devices: String,
}

impl Installed {
    /// The devices it listed.
    pub fn devices(&self) -> Vec<Device> {
        machine::parse_devices(&self.devices)
    }
}

/// The folder a build for `backend` unpacks to.
fn build_dir(backend: Backend) -> PathBuf {
    runtimes().join(format!("b{BUILD}-{}", backend.name()))
}

/// The build for `backend`, if it is here and answered when installed.
pub fn installed(backend: Backend) -> Option<Installed> {
    let folder = build_dir(backend);
    let installed: Installed =
        serde_json::from_slice(&std::fs::read(folder.join("installed.json")).ok()?).ok()?;
    (installed.build == BUILD && installed.server.is_file()).then_some(installed)
}

/// Why the build for `backend` failed to install, if it did.
pub fn runtime_failed(backend: Backend) -> Option<String> {
    let shelf = shelf();
    shelf.states.iter().find_map(|(key, state)| match state {
        State::Failed(why) if key == &format!("runtime/{}", backend.name()) => Some(why.clone()),
        State::Paused { why, .. } if key == &format!("runtime/{}", backend.name()) => Some(
            why.clone()
                .unwrap_or_else(|| "The llama.cpp download was paused.".into()),
        ),
        _ => None,
    })
}

/// How far the build for `backend` has got.
pub fn runtime_state(backend: Backend) -> State {
    if installed(backend).is_some() {
        return State::Ready;
    }
    shelf()
        .states
        .get(&format!("runtime/{}", backend.name()))
        .cloned()
        .unwrap_or(State::None)
}

/// Download, check and unpack a llama.cpp build, and run it once.
fn install(runtime: &Runtime, key: &str, stop: &AtomicBool) -> Result<(), Stop> {
    let failed = Stop::Failed;
    let downloads = runtimes().join("downloads");
    let assets: Vec<_> = [Some(runtime.archive), runtime.extra]
        .into_iter()
        .flatten()
        .collect();
    let mut archives = Vec::new();
    for asset in &assets {
        let dest = downloads.join(asset.name);
        if !on_disk(&dest, asset.size) {
            fetch(&asset.url(), asset.size, asset.sha256, &dest, key, stop)?;
        }
        archives.push(dest);
    }
    set(key, State::Checking { got: 1, total: 1 });
    let folder = build_dir(runtime.backend);
    let fresh = folder.with_extension("new");
    let _ = std::fs::remove_dir_all(&fresh);
    std::fs::create_dir_all(&fresh).map_err(|e| failed(e.to_string()))?;
    for archive in &archives {
        unpack(archive, &fresh).map_err(failed)?;
    }
    let server = find_server(&fresh)
        .ok_or_else(|| failed("The llama.cpp download has no llama-server in it.".into()))?;
    crate::update::make_runnable(&server);
    let version = run(&server, &["--version"]).map_err(failed)?;
    if !version.contains(&format!("build {BUILD}")) {
        return Err(failed(format!(
            "The llama.cpp download is not build {BUILD}: {}",
            version.lines().next().unwrap_or_default()
        )));
    }
    let devices = run(&server, &["--list-devices"]).map_err(failed)?;
    let _ = std::fs::remove_dir_all(&folder);
    std::fs::rename(&fresh, &folder).map_err(|e| failed(e.to_string()))?;
    let installed = Installed {
        backend: runtime.backend,
        build: BUILD,
        server: folder.join(server.strip_prefix(&fresh).unwrap_or(&server)),
        devices,
    };
    let text = serde_json::to_vec_pretty(&installed).map_err(|e| failed(e.to_string()))?;
    std::fs::write(folder.join("installed.json"), text).map_err(|e| failed(e.to_string()))?;
    for archive in archives {
        let _ = std::fs::remove_file(archive);
    }
    let _ = std::fs::remove_dir(&downloads);
    keep_out_of_backups(&runtimes());
    Ok(())
}

/// Unpack a `.zip` or `.tar.gz` into `dest`, refusing any entry that would
/// land outside it, a symlink among them.
fn unpack(archive: &Path, dest: &Path) -> Result<(), String> {
    let file = std::fs::File::open(archive).map_err(|e| e.to_string())?;
    let name = archive.to_string_lossy();
    if name.ends_with(".zip") {
        let mut zip = zip::ZipArchive::new(file).map_err(|e| e.to_string())?;
        for i in 0..zip.len() {
            let mut item = zip.by_index(i).map_err(|e| e.to_string())?;
            let Some(inside) = item.enclosed_name() else {
                return Err(format!("{name} has a file outside its folder."));
            };
            let target = dest.join(inside);
            if item.is_dir() {
                std::fs::create_dir_all(&target).map_err(|e| e.to_string())?;
                continue;
            }
            if let Some(parent) = target.parent() {
                std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
            }
            let mut out = std::fs::File::create(&target).map_err(|e| e.to_string())?;
            std::io::copy(&mut item, &mut out).map_err(|e| e.to_string())?;
        }
        return Ok(());
    }
    let mut tar = tar::Archive::new(flate2::read::GzDecoder::new(file));
    for item in tar.entries().map_err(|e| e.to_string())? {
        let mut item = item.map_err(|e| e.to_string())?;
        let path = item.path().map_err(|e| e.to_string())?.into_owned();
        let kind = item.header().entry_type();
        if kind.is_hard_link() || !stays_inside(&path) {
            return Err(format!("{name} has a file outside its folder."));
        }
        if kind.is_symlink() {
            let target = item
                .link_name()
                .map_err(|e| e.to_string())?
                .map(|t| t.into_owned())
                .unwrap_or_default();
            // A library's other names, beside it: nothing reaching out.
            let beside = path.parent().map_or(target.clone(), |p| p.join(&target));
            if target.is_absolute() || !stays_inside(&beside) {
                return Err(format!("{name} links outside its folder."));
            }
        }
        if !item.unpack_in(dest).map_err(|e| e.to_string())? {
            return Err(format!("{name} has a file outside its folder."));
        }
    }
    Ok(())
}

/// Whether a relative path stays inside the folder it is relative to.
fn stays_inside(path: &Path) -> bool {
    let mut depth = 0i32;
    for part in path.components() {
        match part {
            std::path::Component::Normal(_) => depth += 1,
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                depth -= 1;
                if depth < 0 {
                    return false;
                }
            }
            _ => return false,
        }
    }
    true
}

/// `llama-server` wherever the archive put it.
fn find_server(dir: &Path) -> Option<PathBuf> {
    let name = if cfg!(windows) {
        "llama-server.exe"
    } else {
        "llama-server"
    };
    let mut stack = vec![dir.to_path_buf()];
    while let Some(folder) = stack.pop() {
        for item in std::fs::read_dir(&folder).ok()?.flatten() {
            let path = item.path();
            if path.is_dir() {
                stack.push(path);
            } else if item.file_name() == name {
                return Some(path);
            }
        }
    }
    None
}

/// Run `program` with `args` and what it printed, both streams, stopping it
/// if it does not finish in time.
pub(crate) fn run(program: &Path, args: &[&str]) -> Result<String, String> {
    let mut command = std::process::Command::new(program);
    command
        .args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    let mut child = command
        .spawn()
        .map_err(|e| format!("llama.cpp would not start: {e}"))?;
    let readers: Vec<_> = [
        child
            .stdout
            .take()
            .map(|s| Box::new(s) as Box<dyn Read + Send>),
        child
            .stderr
            .take()
            .map(|s| Box::new(s) as Box<dyn Read + Send>),
    ]
    .into_iter()
    .flatten()
    .map(|mut stream| {
        std::thread::spawn(move || {
            let mut text = String::new();
            let _ = stream.read_to_string(&mut text);
            text
        })
    })
    .collect();
    let started = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if started.elapsed() < SMOKE_TEST => {
                std::thread::sleep(Duration::from_millis(50));
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return Err("llama.cpp did not answer in time.".into());
            }
        }
    };
    let said: String = readers
        .into_iter()
        .filter_map(|reader| reader.join().ok())
        .collect::<Vec<_>>()
        .join("\n");
    if !status.success() {
        let last = said
            .lines()
            .rev()
            .find(|l| !l.trim().is_empty())
            .unwrap_or_default();
        return Err(format!("llama.cpp stopped with {status}: {last}"));
    }
    Ok(said)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mcp_auth::tests::{Request, serve};
    use crate::store::tests::{DATA_DIR, Scratch};

    /// A file the fake serves, with what the catalog would say of it.
    struct Fake {
        body: String,
        sha256: String,
    }

    fn fake(len: usize) -> Fake {
        let body: String = (0..len).map(|i| (b'a' + (i % 26) as u8) as char).collect();
        let sha256 = hex(ring::digest::digest(&ring::digest::SHA256, body.as_bytes()).as_ref());
        Fake { body, sha256 }
    }

    /// Answers a GET for the file, honouring a Range unless told not to.
    fn answer(
        body: &str,
        request: &Request,
        ranges: bool,
    ) -> (u16, Vec<(&'static str, String)>, String) {
        let range = request
            .headers
            .get("range")
            .and_then(|r| r.strip_prefix("bytes="))
            .and_then(|r| r.strip_suffix('-'))
            .and_then(|from| from.parse::<usize>().ok());
        match range {
            Some(from) if ranges => (
                206,
                vec![(
                    "Content-Range",
                    format!("bytes {from}-{}/{}", body.len() - 1, body.len()),
                )],
                body[from..].to_owned(),
            ),
            _ => (200, Vec::new(), body.to_owned()),
        }
    }

    fn scratch(name: &str) -> (std::sync::MutexGuard<'static, ()>, Scratch) {
        let lock = DATA_DIR.lock().unwrap_or_else(PoisonError::into_inner);
        let scratch = Scratch::new(name);
        // SAFETY: tests that set the data folder hold DATA_DIR.
        unsafe { std::env::set_var("AGENT_DATA_DIR", scratch.as_path()) };
        (lock, scratch)
    }

    fn fetched(url: &str, file: &Fake, dest: &Path) -> Result<(), Stop> {
        fetch(
            url,
            file.body.len() as u64,
            &file.sha256,
            dest,
            "test",
            &AtomicBool::new(false),
        )
    }

    #[test]
    fn a_part_left_from_before_is_carried_on_with_a_range() {
        let (_lock, scratch) = scratch("shelf-resume");
        let file = fake(3_000);
        let body = file.body.clone();
        let asked = Arc::new(Mutex::new(Vec::new()));
        let seen = asked.clone();
        let base = serve(move |request| {
            seen.lock()
                .unwrap()
                .push(request.headers.get("range").cloned());
            answer(&body, request, true)
        });
        let dest = scratch.as_path().join("m.gguf");
        std::fs::write(beside(&dest, "part"), &file.body[..1_000]).unwrap();
        let note = Note {
            url: format!("{base}/m.gguf"),
            size: 3_000,
            sha256: file.sha256.clone(),
        };
        std::fs::write(
            beside(&dest, "part.json"),
            serde_json::to_vec(&note).unwrap(),
        )
        .unwrap();
        fetched(&format!("{base}/m.gguf"), &file, &dest).unwrap();
        assert_eq!(std::fs::read_to_string(&dest).unwrap(), file.body);
        assert!(!beside(&dest, "part").exists());
        assert!(!beside(&dest, "part.json").exists());
        assert_eq!(*asked.lock().unwrap(), [Some("bytes=1000-".to_owned())]);
    }

    #[test]
    fn a_server_that_ignores_the_range_starts_the_file_over() {
        let (_lock, scratch) = scratch("shelf-whole");
        let file = fake(2_000);
        let body = file.body.clone();
        let base = serve(move |request| answer(&body, request, false));
        let dest = scratch.as_path().join("m.gguf");
        // A part whose bytes are not the file's: kept, it would fail the hash.
        std::fs::write(beside(&dest, "part"), "x".repeat(500)).unwrap();
        let note = Note {
            url: String::new(),
            size: 2_000,
            sha256: file.sha256.clone(),
        };
        std::fs::write(
            beside(&dest, "part.json"),
            serde_json::to_vec(&note).unwrap(),
        )
        .unwrap();
        fetched(&format!("{base}/m.gguf"), &file, &dest).unwrap();
        assert_eq!(std::fs::read_to_string(&dest).unwrap(), file.body);
    }

    #[test]
    fn a_file_whose_hash_is_wrong_is_thrown_away() {
        let (_lock, scratch) = scratch("shelf-damaged");
        let file = fake(1_500);
        let body = file.body.replacen('a', "b", 1);
        let base = serve(move |request| answer(&body, request, true));
        let dest = scratch.as_path().join("m.gguf");
        let result = fetched(&format!("{base}/m.gguf"), &file, &dest);
        assert_eq!(result, Err(Stop::Failed(DAMAGED.into())));
        assert!(!dest.exists());
        assert!(!beside(&dest, "part").exists());
    }

    #[test]
    fn a_page_where_the_file_was_says_it_moved() {
        let (_lock, scratch) = scratch("shelf-moved");
        let file = fake(1_500);
        let base = serve(|request| match request.target.as_str() {
            "/page" => (
                200,
                vec![("Content-Type", "text/html; charset=utf-8".into())],
                "<html>Sign in</html>".into(),
            ),
            _ => (404, Vec::new(), "Entry not found".into()),
        });
        let dest = scratch.as_path().join("m.gguf");
        assert_eq!(
            fetched(&format!("{base}/gone"), &file, &dest),
            Err(Stop::Failed(MOVED.into()))
        );
        assert_eq!(
            fetched(&format!("{base}/page"), &file, &dest),
            Err(Stop::Failed(MOVED.into()))
        );
    }

    #[test]
    fn a_resume_asks_the_resolve_address_again_and_its_range_follows_the_redirect() {
        let (_lock, scratch) = scratch("shelf-redirect");
        let file = fake(4_000);
        let body = file.body.clone();
        let asked = Arc::new(Mutex::new(Vec::new()));
        let seen = asked.clone();
        let base = Arc::new(OnceLock::<String>::new());
        let here = base.clone();
        let addr = serve(move |request| {
            let base = here.get().unwrap();
            seen.lock().unwrap().push((
                request.target.clone(),
                request.headers.get("range").cloned(),
            ));
            match request.target.as_str() {
                // A signed copy lasts an hour: one kept from before is refused.
                "/signed/old" => (403, Vec::new(), "expired".into()),
                "/resolve/m.gguf" => (
                    302,
                    vec![("Location", format!("{base}/signed/new"))],
                    String::new(),
                ),
                _ => answer(&body, request, true),
            }
        });
        base.set(addr.clone()).unwrap();
        let dest = scratch.as_path().join("m.gguf");
        std::fs::write(beside(&dest, "part"), &file.body[..2_500]).unwrap();
        let note = Note {
            url: format!("{addr}/resolve/m.gguf"),
            size: 4_000,
            sha256: file.sha256.clone(),
        };
        std::fs::write(
            beside(&dest, "part.json"),
            serde_json::to_vec(&note).unwrap(),
        )
        .unwrap();
        fetched(&format!("{addr}/resolve/m.gguf"), &file, &dest).unwrap();
        assert_eq!(std::fs::read_to_string(&dest).unwrap(), file.body);
        let range = Some("bytes=2500-".to_owned());
        assert_eq!(
            *asked.lock().unwrap(),
            [
                ("/resolve/m.gguf".to_owned(), range.clone()),
                ("/signed/new".to_owned(), range)
            ]
        );
    }

    /// A server whose first answer promises the whole file and closes part
    /// way, as a dropped connection does; later ones honour the Range.
    fn dropping(body: String, cut: usize) -> String {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        std::thread::spawn(move || {
            for (n, stream) in listener.incoming().enumerate() {
                let mut stream = stream.unwrap();
                let mut head = Vec::new();
                let mut byte = [0u8; 1];
                while !head.ends_with(b"\r\n\r\n") && stream.read(&mut byte).unwrap_or(0) == 1 {
                    head.push(byte[0]);
                }
                let head = String::from_utf8_lossy(&head).to_ascii_lowercase();
                let from = head
                    .lines()
                    .find_map(|l| l.strip_prefix("range: bytes="))
                    .and_then(|r| r.trim().trim_end_matches('-').parse::<usize>().ok());
                let reply = match (n, from) {
                    (0, _) => format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                        body.len(),
                        &body[..cut]
                    ),
                    (_, Some(from)) => format!(
                        "HTTP/1.1 206 Partial\r\nContent-Range: bytes {from}-{}/{}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                        body.len() - 1,
                        body.len(),
                        body.len() - from,
                        &body[from..]
                    ),
                    (_, None) => format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    ),
                };
                let _ = stream.write_all(reply.as_bytes());
            }
        });
        base
    }

    #[test]
    fn a_dropped_connection_carries_on_from_where_it_stopped() {
        let (_lock, scratch) = scratch("shelf-drop");
        let file = fake(200_000);
        let base = dropping(file.body.clone(), 70_000);
        let dest = scratch.as_path().join("m.gguf");
        fetched(&format!("{base}/m.gguf"), &file, &dest).unwrap();
        assert_eq!(std::fs::read_to_string(&dest).unwrap(), file.body);
    }

    #[test]
    fn no_internet_before_the_first_byte_says_so_and_keeps_nothing() {
        let (_lock, scratch) = scratch("shelf-offline");
        let file = fake(1_000);
        // Nothing listens on port 9 here.
        let dest = scratch.as_path().join("m.gguf");
        let result = fetched("http://127.0.0.1:9/m.gguf", &file, &dest);
        assert_eq!(result, Err(Stop::Failed(NO_INTERNET.into())));
        assert!(!beside(&dest, "part").exists());
        assert!(!beside(&dest, "part.json").exists());
    }

    #[test]
    fn a_quit_part_way_shows_as_paused_with_how_far_it_got() {
        let (_lock, _scratch) = scratch("shelf-left");
        let entry = catalog::entry("bonsai-2-4b").unwrap();
        let model = path(entry, &entry.file);
        std::fs::create_dir_all(model.parent().unwrap()).unwrap();
        std::fs::File::create(beside(&model, "part"))
            .unwrap()
            .set_len(4_000_000)
            .unwrap();
        let note = Note {
            url: entry.file.url(),
            size: entry.file.size,
            sha256: entry.file.sha256.into(),
        };
        std::fs::write(
            beside(&model, "part.json"),
            serde_json::to_vec(&note).unwrap(),
        )
        .unwrap();
        let left = left_from_before();
        assert_eq!(
            left,
            [(
                key(entry, &entry.file),
                State::Paused {
                    got: 4_000_000,
                    total: entry.file.size,
                    why: None,
                }
            )]
        );
        assert_eq!(
            left[0].1.label().as_deref(),
            Some("Paused · 4 MB of 1.2 GB")
        );
    }

    #[test]
    fn a_file_counts_as_here_only_at_its_full_size() {
        let (_lock, _scratch) = scratch("shelf-ready");
        let entry = catalog::entry("bonsai-2-4b").unwrap();
        assert_eq!(state(entry, &entry.file), State::None);
        assert!(ready_ids().is_empty());
        let model = path(entry, &entry.file);
        std::fs::create_dir_all(model.parent().unwrap()).unwrap();
        let file = std::fs::File::create(&model).unwrap();
        file.set_len(entry.file.size - 1).unwrap();
        assert_eq!(state(entry, &entry.file), State::None);
        file.set_len(entry.file.size).unwrap();
        assert_eq!(state(entry, &entry.file), State::Ready);
        assert_eq!(ready_ids(), ["bonsai-2-4b"]);
        assert!(!eyes_ready(entry));
    }

    fn tar_gz(entries: &[(&str, Option<&str>)]) -> Vec<u8> {
        let mut builder = tar::Builder::new(flate2::write::GzEncoder::new(
            Vec::new(),
            flate2::Compression::fast(),
        ));
        for (path, link) in entries {
            let mut header = tar::Header::new_gnu();
            match link {
                Some(target) => {
                    header.set_entry_type(tar::EntryType::Symlink);
                    header.set_size(0);
                    header.set_cksum();
                    builder.append_link(&mut header, path, target).unwrap();
                }
                None => {
                    header.set_size(2);
                    header.set_mode(0o755);
                    header.set_cksum();
                    builder.append_data(&mut header, path, &b"ok"[..]).unwrap();
                }
            }
        }
        builder.into_inner().unwrap().finish().unwrap()
    }

    #[test]
    #[cfg_attr(
        windows,
        ignore = "a symlink on Windows needs Developer Mode; the tar builds are for macOS and Linux"
    )]
    fn a_build_keeps_its_library_links_and_refuses_one_reaching_out() {
        let (_lock, scratch) = scratch("shelf-unpack");
        let good = scratch.as_path().join("good.tar.gz");
        std::fs::write(
            &good,
            tar_gz(&[
                ("llama-b1/llama-server", None),
                ("llama-b1/libllama.0.7.0.dylib", None),
                ("llama-b1/libllama.0.dylib", Some("libllama.0.7.0.dylib")),
            ]),
        )
        .unwrap();
        let into = scratch.as_path().join("good");
        std::fs::create_dir_all(&into).unwrap();
        unpack(&good, &into).unwrap();
        assert!(into.join("llama-b1/libllama.0.dylib").exists());
        // Windows looks for llama-server.exe, which this archive lacks.
        let found = (!cfg!(windows)).then(|| into.join("llama-b1/llama-server"));
        assert_eq!(find_server(&into), found);
        let bad = scratch.as_path().join("bad.tar.gz");
        std::fs::write(
            &bad,
            tar_gz(&[("llama-b1/libevil.dylib", Some("../../../etc/passwd"))]),
        )
        .unwrap();
        let into = scratch.as_path().join("bad");
        std::fs::create_dir_all(&into).unwrap();
        assert!(unpack(&bad, &into).unwrap_err().contains("outside"));
        assert!(stays_inside(Path::new("a/b/../c")));
        assert!(!stays_inside(Path::new("a/../../c")));
        assert!(!stays_inside(Path::new("/etc/passwd")));
    }

    #[test]
    fn the_progress_reads_as_a_share_and_a_label() {
        let getting = State::Getting {
            got: 1_400_000_000,
            total: 5_600_000_000,
        };
        assert_eq!(getting.share(), Some(0.25));
        assert_eq!(getting.label().as_deref(), Some("1.4 GB of 5.6 GB"));
        assert!(getting.moving());
        let full = State::Paused {
            got: 1,
            total: 2,
            why: Some(DISK_FULL.into()),
        };
        assert_eq!(full.label().as_deref(), Some(DISK_FULL));
        assert!(!full.moving());
        assert_eq!(State::Ready.label(), None);
    }
}
