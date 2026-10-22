//! The llama.cpp server a model on this computer runs in.
//!
//! One model is loaded at a time: there is one graphics chip and, on most
//! computers, room for one model. A request for another stops the server
//! and starts one with the new model; requests wait in line, first come
//! first served, and a chat that has to wait is told so. The window and the
//! terminal share the one server: `models/runtime.json` names it (its pid,
//! port and key), and `models/turn.lock` is the line between processes.
//!
//! The server listens on 127.0.0.1 at a port the system picks, and wants a
//! key made for it, so nothing else on the machine can use it; `--offline`
//! keeps it from fetching anything itself. It is stopped after ten idle
//! minutes and when memory runs short, never while it is answering, and it
//! goes with the process that started it: a Job Object on Windows, the
//! parent-death signal on Linux, and on macOS a stop on the way out, with a
//! stale one found by its pid on the next start. A backend that will not
//! load the model (no memory on the card, a driver that fails) is given up
//! for the next one -- CUDA, then Vulkan, then the processor -- once, and
//! remembered.

use super::catalog::{BUILD, Entry};
use super::fit::{self, memory};
use super::machine::{self, Backend, Machine};
use super::shelf::{self, Installed};
use crate::cancel::Cancel;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, VecDeque};
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, OnceLock, PoisonError};
use std::time::{Duration, Instant, SystemTime};

/// How long a model gets to load: a big one off a slow disk takes a while.
const READY_TIME: Duration = Duration::from_secs(120);
/// A server unused this long is stopped, giving its memory back.
const IDLE: Duration = Duration::from_secs(10 * 60);
/// Free memory below which the server is let go when it is not answering.
const LOW_MEMORY: u64 = 1_000_000_000;
/// How long memory has to stay that low first.
const LOW_FOR: Duration = Duration::from_secs(30);
/// How often the watcher looks.
const WATCH_EVERY: Duration = Duration::from_secs(5);
/// Lines of the server's log kept for saying why it failed.
const TAIL: usize = 64;

/// Said to a chat that waits for another to finish with the server.
pub const WAITING: &str = "Waiting for the other chat to finish…";
/// Said once when a graphics card would not load the model.
pub const ON_THE_PROCESSOR: &str = "Running on the processor: the graphics card could not load it.";

/// A server ready to answer, and the turn that holds it until the answer
/// is done.
pub struct Live {
    /// `http://127.0.0.1:<port>/v1`.
    pub endpoint: String,
    pub key: String,
    /// The context it was started with, in tokens.
    pub context: u64,
    pub turn: Turn,
}

/// A place at the front of the line; the next request goes when it drops.
pub struct Turn {
    ticket: u64,
    _across: Option<std::fs::File>,
}

impl Turn {
    /// A turn with no line behind it, for tests that stand a server in.
    #[cfg(test)]
    pub(crate) fn stand_in() -> Turn {
        let ticket = {
            let mut line = line();
            line.next += 1;
            let ticket = line.next;
            line.waiting.push_back(ticket);
            ticket
        };
        Turn {
            ticket,
            _across: None,
        }
    }
}

impl Drop for Turn {
    fn drop(&mut self) {
        let mut line = line();
        line.waiting.retain(|t| *t != self.ticket);
        line.last_used = Instant::now();
        drop(line);
        moved().notify_all();
    }
}

struct Line {
    waiting: VecDeque<u64>,
    next: u64,
    last_used: Instant,
}

fn line() -> MutexGuard<'static, Line> {
    static LINE: OnceLock<Mutex<Line>> = OnceLock::new();
    LINE.get_or_init(|| {
        Mutex::new(Line {
            waiting: VecDeque::new(),
            next: 0,
            last_used: Instant::now(),
        })
    })
    .lock()
    .unwrap_or_else(PoisonError::into_inner)
}

fn moved() -> &'static Condvar {
    static MOVED: Condvar = Condvar::new();
    &MOVED
}

/// Wait for the front of the line, here and in any other Letronna.
fn take_turn(notes: &dyn Fn(String), cancel: &Cancel) -> Result<Turn, String> {
    let ticket = {
        let mut line = line();
        line.next += 1;
        let ticket = line.next;
        line.waiting.push_back(ticket);
        ticket
    };
    let mut told = false;
    let mut line = line();
    while line.waiting.front() != Some(&ticket) {
        if !told {
            told = true;
            notes(WAITING.into());
        }
        if cancel.is_set() {
            line.waiting.retain(|t| *t != ticket);
            drop(line);
            moved().notify_all();
            return Err("Stopped.".into());
        }
        line = moved()
            .wait_timeout(line, Duration::from_millis(200))
            .unwrap_or_else(PoisonError::into_inner)
            .0;
    }
    drop(line);
    let mut turn = Turn {
        ticket,
        _across: None,
    };
    let path = shelf::dir().join("turn.lock");
    let _ = std::fs::create_dir_all(shelf::dir());
    let Ok(file) = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&path)
    else {
        // Without the file the line is this process's alone, which is
        // still one answer at a time here.
        return Ok(turn);
    };
    loop {
        match file.try_lock() {
            Ok(()) => break,
            Err(std::fs::TryLockError::WouldBlock) => {
                if !told {
                    told = true;
                    notes(WAITING.into());
                }
                if cancel.sleep(Duration::from_millis(100)) {
                    return Err("Stopped.".into());
                }
            }
            Err(std::fs::TryLockError::Error(_)) => return Ok(turn),
        }
    }
    turn._across = Some(file);
    Ok(turn)
}

/// Whether no chat holds or waits for the server in this process.
pub fn idle() -> bool {
    line().waiting.is_empty()
}

/// What `models/runtime.json` says of the server running.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct Record {
    pid: u32,
    port: u16,
    key: String,
    model: String,
    photos: bool,
    context: u64,
    build: u32,
    backend: Backend,
    server: PathBuf,
}

fn record_path() -> PathBuf {
    shelf::dir().join("runtime.json")
}

fn read_record() -> Option<Record> {
    serde_json::from_slice(&std::fs::read(record_path()).ok()?).ok()
}

fn write_record(record: Option<&Record>) {
    let path = record_path();
    let Some(record) = record else {
        let _ = std::fs::remove_file(path);
        return;
    };
    let Ok(text) = serde_json::to_vec_pretty(record) else {
        return;
    };
    let fresh = path.with_extension(format!("{}.tmp", std::process::id()));
    if std::fs::write(&fresh, text).is_ok() {
        let _ = std::fs::rename(&fresh, &path);
    }
}

/// The server this process started, while it runs.
struct Running {
    child: Child,
    #[cfg(windows)]
    _tree: Option<crate::jobs::Tree>,
}

fn running() -> MutexGuard<'static, Option<Running>> {
    static RUNNING: Mutex<Option<Running>> = Mutex::new(None);
    RUNNING.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Whether a server, this process's or another Letronna's, has `id` loaded.
/// Starts nothing.
pub fn serving(id: &str) -> bool {
    read_record().is_some_and(|r| r.model == id && alive(&r))
}

/// Whether the server for `id` reads photos too.
pub fn serving_photos(id: &str) -> bool {
    read_record().is_some_and(|r| r.model == id && r.photos && alive(&r))
}

/// Stop the server if it has `id` loaded, as before its file is deleted.
pub fn let_go(id: &str) {
    if read_record().is_some_and(|r| r.model == id) {
        stop();
    }
}

/// Stop the server, this process's or one another Letronna left running.
pub fn stop() {
    let mine = running().take();
    if let Some(mut mine) = mine {
        end(&mut mine.child);
    }
    if let Some(record) = read_record() {
        if alive(&record) {
            kill(record.pid);
        }
        write_record(None);
    }
}

/// On the way out: stop the server this process started, unless another
/// Letronna is answering with it, which keeps it (on macOS; elsewhere the
/// system ends it with this process).
pub fn quit() {
    let Some(mut mine) = running().take() else {
        return;
    };
    if another_answering() {
        std::mem::forget(mine);
        return;
    }
    let pid = mine.child.id();
    end(&mut mine.child);
    if read_record().is_some_and(|r| r.pid == pid) {
        write_record(None);
    }
}

/// Whether another process holds the line to the server.
fn another_answering() -> bool {
    if !idle() {
        return false;
    }
    let Ok(file) = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(shelf::dir().join("turn.lock"))
    else {
        return false;
    };
    match file.try_lock() {
        Ok(()) => {
            let _ = file.unlock();
            false
        }
        Err(std::fs::TryLockError::WouldBlock) => true,
        Err(std::fs::TryLockError::Error(_)) => false,
    }
}

fn end(child: &mut Child) {
    let _ = child.kill();
    let _ = child.wait();
}

/// Whether the pid in a record is still our llama-server, not a process
/// that took its number since.
fn alive(record: &Record) -> bool {
    let pid = sysinfo::Pid::from_u32(record.pid);
    let mut system = sysinfo::System::new();
    system.refresh_processes_specifics(
        sysinfo::ProcessesToUpdate::Some(&[pid]),
        sysinfo::ProcessRefreshKind::new().with_exe(sysinfo::UpdateKind::Always),
    );
    system
        .process(pid)
        .and_then(|p| p.exe())
        .is_some_and(|exe| same_file(exe, &record.server))
}

fn same_file(a: &Path, b: &Path) -> bool {
    match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        (Ok(a), Ok(b)) => a == b,
        _ => a == b,
    }
}

fn kill(pid: u32) {
    let pid = sysinfo::Pid::from_u32(pid);
    let mut system = sysinfo::System::new();
    system.refresh_processes(sysinfo::ProcessesToUpdate::Some(&[pid]));
    if let Some(process) = system.process(pid) {
        process.kill();
    }
}

/// The server for `entry`, started or swapped in as needed, with the turn
/// that holds it. `photos` asks for its vision pack loaded too; `notes`
/// hears what a chat should say while it waits.
pub fn ensure(
    entry: &'static Entry,
    photos: bool,
    notes: &dyn Fn(String),
    cancel: &Cancel,
) -> Result<Live, String> {
    let turn = take_turn(notes, cancel)?;
    let missing = || {
        format!(
            "{} is not on this computer any more. Get it again or pick another.",
            entry.name
        )
    };
    let pack = shelf::ready(entry).ok_or_else(missing)?;
    let photos = photos && shelf::eyes_ready(entry);
    if let Some(record) = read_record()
        && record.model == entry.id
        && record.build == BUILD
        && (record.photos || !photos)
        && alive(&record)
        && healthy(record.port)
    {
        return Ok(live(&record, turn));
    }
    stop();
    let machine = machine::now();
    let fit = fit::fit(entry, &machine, photos);
    let room_on_the_card = fit
        .choice
        .gpu
        .and_then(|i| machine.gpus.get(i))
        .is_some_and(|g| g.kind == machine::Kind::Discrete);
    if !room_on_the_card {
        let free = machine::ram_free();
        if fit.need > free {
            return Err(format!(
                "Close some apps: it needs {}, {} is free now.",
                memory(fit.need),
                memory(free)
            ));
        }
    }
    let model = shelf::path(entry, pack);
    let eyes = entry
        .eyes
        .as_ref()
        .filter(|_| photos)
        .map(|eyes| shelf::path(entry, eyes));
    let mut backend = usable(&machine, fit.choice.backend);
    loop {
        let installed = match shelf::installed(backend) {
            Some(installed) => installed,
            None => install(&machine, backend, notes, cancel)?,
        };
        let gpu = fit.choice.gpu.and_then(|i| machine.gpus.get(i));
        let device = machine::device_for(&installed.devices(), backend, gpu);
        let args = launch_args(
            &model,
            eyes.as_deref(),
            &new_key(),
            fit.context,
            device.as_deref(),
            machine.ram_total <= SMALL_MACHINE,
        );
        match start(&installed, &args, entry, photos, fit.context, cancel) {
            Ok(record) => return Ok(live(&record, turn)),
            Err(Start::Stopped) => return Err("Stopped.".into()),
            Err(Start::Failed(why)) => {
                let Some(next) = next_backend(&machine, backend) else {
                    return Err(format!("{} could not start: {why}", entry.name));
                };
                give_up(&machine, backend, &why);
                if next == Backend::Cpu {
                    notes(ON_THE_PROCESSOR.into());
                }
                backend = next;
            }
        }
    }
}

fn live(record: &Record, turn: Turn) -> Live {
    Live {
        endpoint: format!("http://127.0.0.1:{}/v1", record.port),
        key: record.key.clone(),
        context: record.context,
        turn,
    }
}

/// Memory at or under which the server keeps less in host memory: the
/// prompt cache and context checkpoints default to gigabytes.
const SMALL_MACHINE: u64 = 16 * (1 << 30);

/// The arguments llama-server is started with.
pub(crate) fn launch_args(
    model: &Path,
    eyes: Option<&Path>,
    key: &str,
    context: u64,
    device: Option<&str>,
    small: bool,
) -> Vec<String> {
    let mut args: Vec<String> = vec!["-m".into(), model.to_string_lossy().into_owned()];
    if let Some(eyes) = eyes {
        args.extend(["--mmproj".into(), eyes.to_string_lossy().into_owned()]);
    }
    args.extend(
        [
            "--host",
            "127.0.0.1",
            // The system picks a free port, which the log then names.
            "--port",
            "0",
            "--api-key",
            key,
            "-c",
            &context.to_string(),
            // One answer at a time: the default is a slot per guess at load.
            "-np",
            "1",
            "--fit",
            "on",
            "--fit-target",
            "1024",
            "-fa",
            "auto",
            "--no-ui",
            "--offline",
            "--reasoning",
            "off",
            // Let the weights go if Letronna dies without stopping it.
            "--sleep-idle-seconds",
            "600",
        ]
        .map(str::to_owned),
    );
    if let Some(device) = device {
        args.extend(["--device".into(), device.to_owned()]);
    }
    if small {
        args.extend(["--cache-ram", "1024", "--ctx-checkpoints", "8"].map(str::to_owned));
    }
    args
}

fn new_key() -> String {
    let mut bytes = [0u8; 32];
    let _ = ring::rand::SecureRandom::fill(&ring::rand::SystemRandom::new(), &mut bytes);
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Why a start went wrong.
enum Start {
    Stopped,
    Failed(String),
}

/// Start llama-server and wait until it says it is ready.
fn start(
    installed: &Installed,
    args: &[String],
    entry: &Entry,
    photos: bool,
    context: u64,
    cancel: &Cancel,
) -> Result<Record, Start> {
    let key = args
        .iter()
        .position(|a| a == "--api-key")
        .and_then(|i| args.get(i + 1))
        .cloned()
        .unwrap_or_default();
    let mut command = Command::new(&installed.server);
    command
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    if let Some(folder) = installed.server.parent() {
        command.current_dir(folder);
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // Its own group, so a Ctrl-C at the terminal stops the answer and
        // not the server.
        command.process_group(0);
    }
    #[cfg(target_os = "linux")]
    {
        use std::os::unix::process::CommandExt;
        // SAFETY: prctl is async-signal-safe and touches nothing shared.
        unsafe {
            command.pre_exec(|| {
                libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL);
                Ok(())
            });
        }
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    let mut child =
        spawn(command).map_err(|e| Start::Failed(format!("llama-server would not start: {e}")))?;
    #[cfg(windows)]
    let tree = crate::jobs::Tree::of(&child).ok();
    let tail = Arc::new(Mutex::new(VecDeque::new()));
    let (port_tx, port_rx) = std::sync::mpsc::channel();
    if let Some(stderr) = child.stderr.take() {
        let tail = tail.clone();
        std::thread::spawn(move || {
            // Read to the end, or a full pipe would stop the server.
            for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                if let Some(port) = port_in(&line) {
                    let _ = port_tx.send(port);
                }
                let mut tail = tail.lock().unwrap_or_else(PoisonError::into_inner);
                if tail.len() == TAIL {
                    tail.pop_front();
                }
                tail.push_back(line);
            }
        });
    }
    let why = |tail: &Mutex<VecDeque<String>>| {
        let tail = tail.lock().unwrap_or_else(PoisonError::into_inner);
        failure_in(tail.iter().map(String::as_str))
    };
    let started = Instant::now();
    let mut port = None;
    loop {
        if cancel.is_set() {
            end(&mut child);
            return Err(Start::Stopped);
        }
        if let Ok(Some(status)) = child.try_wait() {
            return Err(Start::Failed(
                why(&tail).unwrap_or_else(|| format!("it stopped with {status}")),
            ));
        }
        if started.elapsed() > READY_TIME {
            end(&mut child);
            return Err(Start::Failed(
                why(&tail).unwrap_or_else(|| "it did not load in two minutes".into()),
            ));
        }
        if port.is_none() {
            port = port_rx.try_recv().ok();
        }
        if let Some(port) = port
            && healthy(port)
        {
            let record = Record {
                pid: child.id(),
                port,
                key,
                model: entry.id.to_owned(),
                photos,
                context,
                build: BUILD,
                backend: installed.backend,
                server: installed.server.clone(),
            };
            write_record(Some(&record));
            *running() = Some(Running {
                child,
                #[cfg(windows)]
                _tree: tree,
            });
            watch();
            return Ok(record);
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// Start `command` from a thread that lives as long as the process: Linux
/// sends the parent-death signal when the thread that started a child ends,
/// not the process, so a chat's own thread would take the server with it.
fn spawn(command: Command) -> std::io::Result<Child> {
    type Ask = (Command, std::sync::mpsc::Sender<std::io::Result<Child>>);
    static KEEPER: OnceLock<Mutex<std::sync::mpsc::Sender<Ask>>> = OnceLock::new();
    let keeper = KEEPER.get_or_init(|| {
        let (tx, rx) = std::sync::mpsc::channel::<Ask>();
        std::thread::spawn(move || {
            for (mut command, reply) in rx {
                let _ = reply.send(command.spawn());
            }
        });
        Mutex::new(tx)
    });
    let (reply, answer) = std::sync::mpsc::channel();
    keeper
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .send((command, reply))
        .map_err(|_| std::io::Error::other("the keeper thread is gone"))?;
    answer
        .recv()
        .map_err(|_| std::io::Error::other("the keeper thread is gone"))?
}

/// The port in llama-server's `llama_server: listening on http://127.0.0.1:60151`.
fn port_in(line: &str) -> Option<u16> {
    let (_, rest) = line.split_once("listening on http://127.0.0.1:")?;
    rest.chars()
        .take_while(char::is_ascii_digit)
        .collect::<String>()
        .parse()
        .ok()
}

/// Why llama-server failed, from its last lines: the backend's own words
/// where it ran out of memory or its driver failed.
fn failure_in<'a>(lines: impl DoubleEndedIterator<Item = &'a str>) -> Option<String> {
    const SIGNS: [&str; 6] = [
        "failed to allocate",
        "ErrorOutOfDeviceMemory",
        "out of memory",
        "CUDA error",
        "failed to load model",
        "error:",
    ];
    let lines: Vec<&str> = lines.collect();
    SIGNS.iter().find_map(|sign| {
        lines
            .iter()
            .rev()
            .find(|line| line.contains(sign))
            .map(|line| shorten(line))
    })
}

/// A log line without its timestamp and source prefix.
fn shorten(line: &str) -> String {
    let words: Vec<&str> = line.split_whitespace().collect();
    // "0.01.328.313 E srv  load_model: failed to …": a time, a level, and
    // sometimes a short source before the message.
    let mut skip = 0;
    if words.len() > 2 && words[0].contains('.') && words[1].len() == 1 {
        skip = 2;
        if words.len() > 3 && words[2].len() <= 4 && !words[2].ends_with(':') {
            skip = 3;
        }
    }
    words[skip..].join(" ")
}

fn healthy(port: u16) -> bool {
    static CLIENT: OnceLock<Option<reqwest::blocking::Client>> = OnceLock::new();
    let Some(client) = CLIENT
        .get_or_init(|| {
            reqwest::blocking::Client::builder()
                .timeout(Duration::from_secs(2))
                .no_proxy()
                .build()
                .ok()
        })
        .as_ref()
    else {
        return false;
    };
    client
        .get(format!("http://127.0.0.1:{port}/health"))
        .send()
        .is_ok_and(|r| r.status().as_u16() == 200)
}

/// The backend to take after `backend` fails: CUDA gives way to Vulkan,
/// Vulkan to the processor; Metal has nothing behind it.
fn next_backend(machine: &Machine, backend: Backend) -> Option<Backend> {
    let has = |b| super::catalog::runtime(&machine.os, &machine.arch, b).is_some();
    let next = match backend {
        Backend::Cuda13 | Backend::Cuda12 if has(Backend::Vulkan) && machine.vulkan.is_some() => {
            Backend::Vulkan
        }
        Backend::Cuda13 | Backend::Cuda12 | Backend::Vulkan => Backend::Cpu,
        Backend::Metal | Backend::Cpu => return None,
    };
    has(next).then_some(next)
}

/// `backend`, unless it failed on this computer's driver before; then the
/// next that has not.
fn usable(machine: &Machine, mut backend: Backend) -> Backend {
    let failed = failures();
    while failed.contains_key(&failure_key(machine, backend))
        && let Some(next) = next_backend(machine, backend)
    {
        backend = next;
    }
    backend
}

fn failures_path() -> PathBuf {
    shelf::dir().join("backends.json")
}

fn failures() -> HashMap<String, String> {
    std::fs::read(failures_path())
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

/// A failure is kept against the backend and the driver it failed on, so
/// a driver update tries the card again.
fn failure_key(machine: &Machine, backend: Backend) -> String {
    let driver = machine
        .gpus
        .iter()
        .map(|g| format!("{}:{}", g.name, g.driver.as_deref().unwrap_or("")))
        .collect::<Vec<_>>()
        .join(",");
    format!("{}|{driver}", backend.name())
}

fn give_up(machine: &Machine, backend: Backend, why: &str) {
    let mut failed = failures();
    failed.insert(failure_key(machine, backend), why.to_owned());
    if let Ok(text) = serde_json::to_vec_pretty(&failed) {
        let _ = std::fs::write(failures_path(), text);
    }
}

/// Fetch the build for `backend` now, waiting for it.
fn install(
    machine: &Machine,
    backend: Backend,
    notes: &dyn Fn(String),
    cancel: &Cancel,
) -> Result<Installed, String> {
    notes("Getting llama.cpp for this computer…".into());
    shelf::get_runtime(machine, backend)?;
    loop {
        if let Some(installed) = shelf::installed(backend) {
            return Ok(installed);
        }
        if let Some(why) = shelf::runtime_failed(backend) {
            return Err(why);
        }
        if cancel.is_set() {
            return Err("Stopped.".into());
        }
        cancel.sleep(Duration::from_millis(250));
    }
}

/// Keep an eye on the server this process started: stop it once idle for
/// ten minutes or when memory runs short, and forget it if it died.
fn watch() {
    static WATCHING: Mutex<bool> = Mutex::new(false);
    let mut watching = WATCHING.lock().unwrap_or_else(PoisonError::into_inner);
    if *watching {
        return;
    }
    *watching = true;
    drop(watching);
    std::thread::spawn(|| {
        let mut low_since: Option<Instant> = None;
        loop {
            std::thread::sleep(WATCH_EVERY);
            let mut mine = running();
            let Some(server) = mine.as_mut() else {
                break;
            };
            if !matches!(server.child.try_wait(), Ok(None)) {
                *mine = None;
                if read_record().is_some_and(|r| !alive(&r)) {
                    write_record(None);
                }
                break;
            }
            drop(mine);
            let quiet = idle();
            let unused = line().last_used.elapsed();
            low_since = if machine::ram_free() < LOW_MEMORY {
                low_since.or(Some(Instant::now()))
            } else {
                None
            };
            let short = low_since.is_some_and(|since| since.elapsed() >= LOW_FOR);
            if quiet && (unused >= IDLE || short) {
                stop();
                break;
            }
        }
        *WATCHING.lock().unwrap_or_else(PoisonError::into_inner) = false;
    });
}

/// Whether the computer slept since `wall` and `steady` were read together:
/// the monotonic clock stops while it sleeps and the wall clock does not.
pub fn slept(wall: SystemTime, steady: Instant) -> bool {
    let wall = wall.elapsed().unwrap_or_default();
    wall.saturating_sub(steady.elapsed()) > Duration::from_secs(30)
}

/// What a model's first answer here measured, in tokens a second.
#[derive(Debug, Default, Serialize, Deserialize)]
struct Measured(HashMap<String, f64>);

fn measured_path() -> PathBuf {
    shelf::dir().join("measured.json")
}

fn measured_key(entry: &Entry, backend: Backend, machine: &Machine) -> String {
    let gpu = machine.gpus.first().map_or("", |g| g.name.as_str());
    format!("{}|{}|{}|{gpu}", entry.id, backend.name(), machine.cpu)
}

/// The speed `entry` was measured at on this computer, if it was.
pub fn measured(entry: &Entry, machine: &Machine) -> Option<f64> {
    let backend = machine::pick(machine, entry).backend;
    let all: Measured = serde_json::from_slice(&std::fs::read(measured_path()).ok()?).ok()?;
    all.0.get(&measured_key(entry, backend, machine)).copied()
}

/// Time a short answer from `entry` and keep it; nothing is kept on battery
/// or in low-power mode, where it would undersell the model.
pub fn measure(entry: &'static Entry) -> Result<f64, String> {
    let machine = machine::now();
    if machine.on_battery || machine.low_power {
        return Err("On battery: it is timed once the computer is plugged in.".into());
    }
    let live = ensure(entry, false, &|_| {}, &Cancel::default())?;
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(120))
        .no_proxy()
        .build()
        .map_err(|e| e.to_string())?;
    let mut body = serde_json::json!({
        "messages": [{"role": "user", "content": "Write two sentences about the sea."}],
        "max_tokens": 64,
    });
    entry.shape(&mut body, 64);
    let reply: serde_json::Value = client
        .post(format!("{}/chat/completions", live.endpoint))
        .bearer_auth(&live.key)
        .json(&body)
        .send()
        .and_then(|r| r.json())
        .map_err(|e| format!("The first answer failed: {e}"))?;
    drop(live);
    let speed = reply["timings"]["predicted_per_second"]
        .as_f64()
        .ok_or("The server did not time its answer.")?;
    let backend = read_record().map_or(machine::pick(&machine, entry).backend, |r| r.backend);
    let mut all: Measured = std::fs::read(measured_path())
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default();
    all.0.insert(measured_key(entry, backend, &machine), speed);
    if let Ok(text) = serde_json::to_vec_pretty(&all) {
        let _ = std::fs::write(measured_path(), text);
    }
    Ok(speed)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::on_device::machine::tests::{apple, bare, card, pc};
    use crate::on_device::{Kind, Vendor};

    const GIB: u64 = 1 << 30;

    #[test]
    fn the_server_is_started_on_loopback_with_a_key_one_slot_and_no_web_page() {
        let args = launch_args(
            Path::new("/m/model.gguf"),
            Some(Path::new("/m/eyes.gguf")),
            "k",
            16_384,
            Some("MTL0"),
            true,
        );
        let joined = args.join(" ");
        assert!(joined.starts_with("-m /m/model.gguf --mmproj /m/eyes.gguf --host 127.0.0.1 --port 0 --api-key k -c 16384 -np 1"));
        for flag in [
            "--no-ui",
            "--offline",
            "--reasoning off",
            "--device MTL0",
            "--cache-ram 1024",
            "--ctx-checkpoints 8",
            "--sleep-idle-seconds 600",
        ] {
            assert!(joined.contains(flag), "{flag} in {joined}");
        }
        let big = launch_args(Path::new("/m/model.gguf"), None, "k", 32_768, None, false).join(" ");
        assert!(
            !big.contains("--mmproj") && !big.contains("--device") && !big.contains("--cache-ram")
        );
    }

    #[test]
    fn the_port_is_read_off_the_servers_log() {
        assert_eq!(
            port_in("0.00.661.084 I srv  llama_server: listening on http://127.0.0.1:60151"),
            Some(60151)
        );
        assert_eq!(port_in("llama_server: initializing ..."), None);
    }

    #[test]
    fn a_failure_is_told_in_the_backends_own_words() {
        let log = [
            "0.00.070.152 I srv  llama_server: initializing ...",
            "0.01.100.000 E ggml_vulkan: Device memory allocation of size 4294967296 failed: ErrorOutOfDeviceMemory",
            "0.01.100.100 E llama_model_load: error loading model",
        ];
        assert_eq!(
            failure_in(log.into_iter()).as_deref(),
            Some(
                "ggml_vulkan: Device memory allocation of size 4294967296 failed: ErrorOutOfDeviceMemory"
            )
        );
        assert_eq!(failure_in(["all is well"].into_iter()), None);
    }

    #[test]
    fn a_failing_backend_gives_way_to_the_next_once() {
        let windows = pc(
            "windows",
            32 * GIB,
            vec![card(
                "NVIDIA GeForce RTX 4070",
                Vendor::Nvidia,
                Kind::Discrete,
                12 * GIB,
                Some("580.88"),
            )],
        );
        assert_eq!(
            next_backend(&windows, Backend::Cuda13),
            Some(Backend::Vulkan)
        );
        assert_eq!(next_backend(&windows, Backend::Vulkan), Some(Backend::Cpu));
        assert_eq!(next_backend(&windows, Backend::Cpu), None);
        let mut no_vulkan = windows.clone();
        no_vulkan.vulkan = None;
        assert_eq!(
            next_backend(&no_vulkan, Backend::Cuda12),
            Some(Backend::Cpu)
        );
        let m5 = apple("Apple M5", 16 * GIB, 12_124 << 20);
        assert_eq!(next_backend(&m5, Backend::Metal), None);
        let linux = bare("linux", "x86_64", 16 * GIB);
        assert_eq!(next_backend(&linux, Backend::Vulkan), Some(Backend::Cpu));
    }

    #[test]
    fn a_backend_that_failed_is_skipped_until_the_driver_changes() {
        let _lock = crate::store::tests::DATA_DIR
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let scratch = crate::store::tests::Scratch::new("runtime-failed");
        // SAFETY: tests that set the data folder hold DATA_DIR.
        unsafe { std::env::set_var("AGENT_DATA_DIR", scratch.as_path()) };
        std::fs::create_dir_all(shelf::dir()).unwrap();
        let mut machine = pc(
            "windows",
            32 * GIB,
            vec![card(
                "NVIDIA GeForce RTX 4070",
                Vendor::Nvidia,
                Kind::Discrete,
                12 * GIB,
                Some("580.88"),
            )],
        );
        assert_eq!(usable(&machine, Backend::Vulkan), Backend::Vulkan);
        give_up(&machine, Backend::Vulkan, "ErrorOutOfDeviceMemory");
        assert_eq!(usable(&machine, Backend::Vulkan), Backend::Cpu);
        machine.gpus[0].driver = Some("590.10".into());
        assert_eq!(usable(&machine, Backend::Vulkan), Backend::Vulkan);
    }

    #[test]
    fn a_chat_behind_another_waits_its_turn_and_is_told() {
        let _line = crate::on_device::reach::tests::SEAMS
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let _lock = crate::store::tests::DATA_DIR
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let scratch = crate::store::tests::Scratch::new("runtime-turn");
        // SAFETY: tests that set the data folder hold DATA_DIR.
        unsafe { std::env::set_var("AGENT_DATA_DIR", scratch.as_path()) };
        let first = Turn::stand_in();
        let told = Arc::new(Mutex::new(Vec::new()));
        let heard = told.clone();
        let waiting = std::thread::spawn(move || {
            let notes = move |note: String| heard.lock().unwrap().push(note);
            take_turn(&notes, &Cancel::default()).map(drop)
        });
        std::thread::sleep(Duration::from_millis(300));
        assert!(!waiting.is_finished());
        drop(first);
        waiting.join().unwrap().unwrap();
        assert_eq!(*told.lock().unwrap(), [WAITING.to_owned()]);
    }

    #[test]
    fn a_stopped_chat_leaves_the_line() {
        let _line = crate::on_device::reach::tests::SEAMS
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let first = Turn::stand_in();
        let cancel = Cancel::default();
        let stop = cancel.clone();
        let waiting = std::thread::spawn(move || take_turn(&|_| {}, &cancel).err());
        std::thread::sleep(Duration::from_millis(100));
        stop.set();
        assert_eq!(waiting.join().unwrap().as_deref(), Some("Stopped."));
        drop(first);
        assert!(idle());
    }

    #[test]
    fn a_sleep_shows_as_the_wall_clock_running_ahead() {
        let now = SystemTime::now();
        assert!(!slept(now, Instant::now()));
        assert!(slept(now - Duration::from_secs(600), Instant::now()));
    }

    /// A scratch data folder with Bonsai 2 4B in it, linked from where
    /// `LETRONNA_TEST_MODEL` names it, and the build for this computer.
    fn with_bonsai(name: &str) -> Option<(crate::store::tests::Scratch, &'static Entry)> {
        let model = std::env::var("LETRONNA_TEST_MODEL").ok()?;
        let scratch = crate::store::tests::Scratch::new(name);
        // SAFETY: tests that set the data folder hold DATA_DIR.
        unsafe { std::env::set_var("AGENT_DATA_DIR", scratch.as_path()) };
        let entry = super::super::catalog::entry("bonsai-2-4b").unwrap();
        let dest = shelf::path(entry, &entry.file);
        std::fs::create_dir_all(dest.parent().unwrap()).unwrap();
        std::fs::hard_link(&model, &dest)
            .or_else(|_| std::fs::copy(&model, &dest).map(drop))
            .unwrap();
        Some((scratch, entry))
    }

    #[test]
    #[ignore = "Downloads llama.cpp b11809 from GitHub: cargo test -p letronna-core --release -- --ignored a_real_build"]
    fn a_real_build_installs_and_lists_its_devices() {
        let _lock = crate::store::tests::DATA_DIR
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let scratch = crate::store::tests::Scratch::new("runtime-real-build");
        // SAFETY: tests that set the data folder hold DATA_DIR.
        unsafe { std::env::set_var("AGENT_DATA_DIR", scratch.as_path()) };
        let machine = machine::detect();
        let backend = machine::pick(&machine, &super::super::catalog::CATALOG[0]).backend;
        let installed = install(&machine, backend, &|_| {}, &Cancel::default()).unwrap();
        assert_eq!(installed.build, BUILD);
        assert!(installed.server.is_file());
        if backend != Backend::Cpu {
            assert!(!installed.devices().is_empty(), "{}", installed.devices);
        }
    }

    #[test]
    #[ignore = "Downloads llama.cpp and runs Bonsai 2 4B: LETRONNA_TEST_MODEL=<its .gguf> cargo test -p letronna-core --release -- --ignored a_real_model"]
    fn a_real_model_answers_on_this_computer_and_is_shared() {
        let _line = crate::on_device::reach::tests::SEAMS
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let _lock = crate::store::tests::DATA_DIR
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let Some((_scratch, entry)) = with_bonsai("runtime-real-model") else {
            panic!("set LETRONNA_TEST_MODEL to Ternary-Bonsai-2-4B-Q2_0.gguf");
        };
        let notes = Mutex::new(Vec::new());
        let live = ensure(
            entry,
            false,
            &|n| notes.lock().unwrap().push(n),
            &Cancel::default(),
        )
        .unwrap();
        assert!(live.endpoint.starts_with("http://127.0.0.1:"));
        assert_eq!(live.context, fit::CONTEXT);
        let client = reqwest::blocking::Client::builder()
            .no_proxy()
            .build()
            .unwrap();
        let refused = client
            .get(format!("{}/models", live.endpoint))
            .send()
            .unwrap()
            .status()
            .as_u16();
        assert_eq!(refused, 401);
        let mut body = serde_json::json!({
            "messages": [{"role": "user", "content": "What is 17 times 23? Answer with the number."}],
        });
        entry.shape(&mut body, 64);
        let reply: serde_json::Value = client
            .post(format!("{}/chat/completions", live.endpoint))
            .bearer_auth(&live.key)
            .json(&body)
            .send()
            .unwrap()
            .json()
            .unwrap();
        let text = reply["choices"][0]["message"]["content"]
            .as_str()
            .unwrap_or_default();
        assert!(text.contains("391"), "{reply}");
        let port = live.endpoint.clone();
        drop(live);
        assert!(serving(entry.id));
        // A second request finds the same server.
        let again = ensure(entry, false, &|_| {}, &Cancel::default()).unwrap();
        assert_eq!(again.endpoint, port);
        drop(again);
        let_go(entry.id);
        assert!(!serving(entry.id));
        assert!(read_record().is_none());
    }
}
