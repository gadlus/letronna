//! One plugin whose code may run: the queue its messages wait in, the host
//! that runs it once something needs it, and the answers that come back.
//!
//! The queue is made with the plugin, not with its host, so an event can be
//! queued before anything has started and the host reads it once it has;
//! the caller never waits for a start. It holds `QUEUE` messages: a plugin
//! that falls that far behind has what it is sent dropped, and is told on,
//! rather than holding up the agent or the window. Each message carries the
//! epoch it was sent in, which a stop moves on, so what was queued for a
//! host that was stopped is passed over by the next rather than done late
//! for nobody.
//!
//! A host that ends without being asked to has failed. Its plugin is started
//! again the next time it is needed, after a wait that doubles with each
//! failure, and one that fails `CRASHES` times in `CRASH_WINDOW` is left
//! off until the user reloads it, so a plugin that dies on start does not
//! cost a start on every event.

use super::{Code, SettingKind, ui};
use crate::cancel::{Cancel, Waker};
use serde_json::{Map, Value, json};
use std::collections::HashMap;
use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, SyncSender, TrySendError};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

/// Messages that wait for one plugin.
const QUEUE: usize = 64;
/// The bytes of log one plugin keeps before the older half is let go.
const LOG_BYTES: u64 = 256 * 1024;
/// Failures in `CRASH_WINDOW` after which a plugin is left off.
const CRASHES: usize = 5;
const CRASH_WINDOW: Duration = Duration::from_secs(600);
/// The wait before a start after the first failure, doubling to the last.
const FIRST_RETRY: Duration = Duration::from_secs(1);
const LAST_RETRY: Duration = Duration::from_secs(30);
/// Problems one plugin reports in `PROBLEM_WINDOW`; past that they go only
/// to its log, so a handler failing on every event cannot fill the screen.
const PROBLEMS: usize = 3;
const PROBLEM_WINDOW: Duration = Duration::from_secs(60);
/// The characters a text setting keeps.
const SETTING_CHARS: usize = 2_000;

/// A plugin's running code, as the rest sees it.
pub(super) trait Host: Send {
    /// Stops it soon; what it was doing is dropped.
    fn stop(&self);
}

/// A plugin allowed to run code, whether or not it has started.
pub(crate) struct Loaded {
    pub(super) name: String,
    pub(super) folder: PathBuf,
    /// Its own folder in the data folder: settings, storage and log.
    pub(super) own: PathBuf,
    pub(super) code: Code,
    /// The events it hears, as bits.
    pub(super) events: u32,
    sender: SyncSender<Value>,
    /// What its host reads; the host holds it while it runs, so a host
    /// started after a stop reads only once the one before has let go.
    pub(super) queue: Mutex<Receiver<Value>>,
    host: Mutex<Option<Box<dyn Host>>>,
    /// Whether `host` holds one, read without its lock: a window's thread
    /// asks, and must not wait behind a start.
    alive: AtomicBool,
    /// Counts the hosts started, so one that ends late cannot clear the next.
    generation: AtomicU64,
    /// Moves on at every stop; see `stale`.
    epoch: AtomicU64,
    starting: AtomicBool,
    /// Switched off or taken out: it never starts again.
    retired: AtomicBool,
    waiting: Mutex<HashMap<u64, mpsc::Sender<Answer>>>,
    next: AtomicU64,
    dropped: AtomicU64,
    trouble: Mutex<Trouble>,
    log: Mutex<()>,
}

/// What came back for a request.
enum Answer {
    Said(Value),
    Failed(String),
    Stopped,
}

/// How a request ended, for its caller to put in words.
pub(super) enum Waited {
    /// The plugin's answer, the whole message.
    Said(Value),
    /// It could not be asked or answer: why.
    Failed(String),
    /// The caller's stop came first.
    Stopped,
    /// The time ran out.
    Late,
}

#[derive(Default)]
struct Trouble {
    crashes: Vec<Instant>,
    retry_at: Option<Instant>,
    left_off: bool,
    problems: Vec<Instant>,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

impl Loaded {
    pub(super) fn new(name: String, folder: PathBuf, own: PathBuf, code: Code) -> Arc<Self> {
        let (sender, queue) = mpsc::sync_channel(QUEUE);
        Arc::new(Self {
            events: code.events(),
            name,
            folder,
            own,
            code,
            sender,
            queue: Mutex::new(queue),
            host: Mutex::new(None),
            alive: AtomicBool::new(false),
            generation: AtomicU64::new(0),
            epoch: AtomicU64::new(0),
            starting: AtomicBool::new(false),
            retired: AtomicBool::new(false),
            waiting: Mutex::new(HashMap::new()),
            next: AtomicU64::new(0),
            dropped: AtomicU64::new(0),
            trouble: Mutex::new(Trouble::default()),
            log: Mutex::new(()),
        })
    }

    /// Where a host puts what it hands itself, such as a fetch's answer.
    pub(super) fn sender(&self) -> SyncSender<Value> {
        self.sender.clone()
    }

    pub(super) fn running(&self) -> bool {
        self.alive.load(Ordering::SeqCst)
    }

    /// The epoch messages are sent in now; a host keeps the one it started
    /// in.
    pub(super) fn epoch(&self) -> u64 {
        self.epoch.load(Ordering::SeqCst)
    }

    /// Whether `message` was queued before a stop that came after it, for a
    /// host that started in `since`. A host's own messages carry no epoch.
    pub(super) fn stale(message: &Value, since: u64) -> bool {
        message["epoch"].as_u64().is_some_and(|sent| sent < since)
    }

    /// Starts it now unless it runs; why not, when it cannot be.
    pub(super) fn ensure(self: &Arc<Self>) -> Result<(), String> {
        if self.running() {
            return Ok(());
        }
        // Looking for Python can take seconds the first time; done before
        // the lock, so nothing that only asks whether it runs waits on it.
        if self.code.runtime == super::Runtime::Python {
            super::python::prepare(self)
                .map_err(|why| self.said(&format!("could not start: {why}")))?;
        }
        let mut host = lock(&self.host);
        if host.is_some() {
            return Ok(());
        }
        self.may_start()?;
        let generation = self.generation.fetch_add(1, Ordering::SeqCst) + 1;
        let started = match self.code.runtime {
            #[cfg(feature = "js")]
            super::Runtime::JavaScript => super::js::start(self.clone(), generation),
            #[cfg(not(feature = "js"))]
            super::Runtime::JavaScript => Err("This build of Letronna has no JavaScript.".into()),
            super::Runtime::Python => super::python::start(self.clone(), generation),
        };
        match started {
            Ok(started) => {
                *host = Some(started);
                self.alive.store(true, Ordering::SeqCst);
                Ok(())
            }
            Err(why) => {
                drop(host);
                self.failed(&why);
                Err(self.said(&format!("could not start: {why}")))
            }
        }
    }

    /// Starts it off the caller's thread unless it runs, for an event that
    /// is queued already and must not wait on a start.
    pub(super) fn wake(self: &Arc<Self>) {
        if self.running() || self.starting.swap(true, Ordering::SeqCst) {
            return;
        }
        let plugin = self.clone();
        let spawned = std::thread::Builder::new()
            .name(format!("plugin {} start", self.name))
            .spawn(move || {
                // A plugin that cannot start has said why already.
                let _ = plugin.ensure();
                plugin.starting.store(false, Ordering::SeqCst);
            });
        if spawned.is_err() {
            self.starting.store(false, Ordering::SeqCst);
        }
    }

    /// Why it may not start now, when it may not.
    fn may_start(&self) -> Result<(), String> {
        if self.retired.load(Ordering::SeqCst) {
            return Err(self.said("is switched off."));
        }
        let trouble = lock(&self.trouble);
        if trouble.left_off {
            return Err(self.said(&format!(
                "failed {CRASHES} times in {} minutes, so it is left off until it is reloaded.",
                CRASH_WINDOW.as_secs() / 60
            )));
        }
        if let Some(at) = trouble.retry_at
            && let Some(left) = at.checked_duration_since(Instant::now())
        {
            return Err(self.said(&format!(
                "failed, and starts again in {} s.",
                left.as_secs().max(1)
            )));
        }
        Ok(())
    }

    /// Stops its host, if one runs; it starts again when next needed.
    pub(super) fn stop(&self) {
        // A host that ends now ends as asked, not as a failure.
        self.generation.fetch_add(1, Ordering::SeqCst);
        self.epoch.fetch_add(1, Ordering::SeqCst);
        let host = lock(&self.host).take();
        self.alive.store(false, Ordering::SeqCst);
        if let Some(host) = host {
            host.stop();
        }
        self.fail_waiting(&self.said("was stopped."));
        ui::forget(&self.name);
    }

    /// Stops it for good: it was switched off, taken out or changed.
    pub(super) fn retire(&self) {
        self.retired.store(true, Ordering::SeqCst);
        self.stop();
    }

    /// Its host `generation` ended, with why when it failed. Called on the
    /// host's own thread.
    pub(super) fn ended(self: &Arc<Self>, generation: u64, outcome: Result<(), String>) {
        let mut host = lock(&self.host);
        if self.generation.load(Ordering::SeqCst) != generation {
            return;
        }
        host.take();
        self.alive.store(false, Ordering::SeqCst);
        drop(host);
        let why = match outcome {
            Ok(()) => "ended.".to_owned(),
            Err(why) => why,
        };
        self.fail_waiting(&self.said(&format!("stopped: {why}")));
        self.failed(&why);
        ui::forget(&self.name);
        if self.code.start == super::Start::Launch {
            self.again_later();
        }
    }

    /// Counts a failure and reports it, setting when it may start again.
    fn failed(&self, why: &str) {
        let mut trouble = lock(&self.trouble);
        let now = Instant::now();
        trouble
            .crashes
            .retain(|at| now.duration_since(*at) < CRASH_WINDOW);
        trouble.crashes.push(now);
        let count = trouble.crashes.len();
        trouble.left_off = count >= CRASHES;
        let wait = FIRST_RETRY.saturating_mul(1 << (count - 1).min(5));
        trouble.retry_at = Some(now + wait.min(LAST_RETRY));
        drop(trouble);
        self.problem(&format!("stopped: {why}"));
    }

    /// Starts a plugin that runs with Letronna again once its wait is over.
    fn again_later(self: &Arc<Self>) {
        let Some(at) = lock(&self.trouble).retry_at else {
            return;
        };
        let plugin = self.clone();
        let _ = std::thread::Builder::new()
            .name(format!("plugin {} restart", self.name))
            .spawn(move || {
                std::thread::sleep(at.saturating_duration_since(Instant::now()));
                let _ = plugin.ensure();
            });
    }

    /// Lets the user try again at once: what failed before is forgotten.
    pub(super) fn forgive(&self) {
        *lock(&self.trouble) = Trouble::default();
        self.dropped.store(0, Ordering::Relaxed);
    }

    /// Queues `message` for its host; false when it is too far behind.
    pub(super) fn post(&self, mut message: Value) -> bool {
        message["epoch"] = self.epoch().into();
        match self.sender.try_send(message) {
            Ok(()) => true,
            Err(TrySendError::Full(_)) => {
                if self.dropped.fetch_add(1, Ordering::Relaxed) == 0 {
                    self.problem("is behind, so what it is sent is dropped until it catches up.");
                }
                false
            }
            Err(TrySendError::Disconnected(_)) => false,
        }
    }

    /// Sends `message` as a request, starting the plugin if need be, for its
    /// answer to be waited on; `cancel` ends the wait when set.
    pub(super) fn request(
        self: &Arc<Self>,
        mut message: Value,
        cancel: Option<&Cancel>,
    ) -> Result<Pending, String> {
        self.ensure()?;
        let id = self.next.fetch_add(1, Ordering::Relaxed) + 1;
        let (tell, answers) = mpsc::channel();
        let waker = cancel.map(|cancel| {
            let tell = tell.clone();
            cancel.on_set(move || {
                let _ = tell.send(Answer::Stopped);
            })
        });
        lock(&self.waiting).insert(id, tell);
        message["id"] = id.into();
        if !self.post(message) {
            lock(&self.waiting).remove(&id);
            return Err(self.said("is behind, so this was not given to it."));
        }
        Ok(Pending {
            plugin: self.clone(),
            id,
            answers,
            _waker: waker,
        })
    }

    /// `request`, waited on for `wait`.
    pub(super) fn ask(
        self: &Arc<Self>,
        message: Value,
        wait: Duration,
        cancel: Option<&Cancel>,
    ) -> Waited {
        match self.request(message, cancel) {
            Ok(pending) => pending.wait(Instant::now() + wait),
            Err(why) => Waited::Failed(why),
        }
    }

    fn fail_waiting(&self, why: &str) {
        let waiting = std::mem::take(&mut *lock(&self.waiting));
        for (_, tell) in waiting {
            let _ = tell.send(Answer::Failed(why.to_owned()));
        }
    }

    /// What its code sent: an answer to a request, or something to show.
    /// Called on the host's thread.
    pub(super) fn heard(&self, message: Value) {
        if let Some(id) = message.get("reply").and_then(Value::as_u64) {
            if let Some(tell) = lock(&self.waiting).remove(&id) {
                let _ = tell.send(Answer::Said(message));
            }
            return;
        }
        let text = message["text"].as_str().unwrap_or_default();
        match message["do"].as_str().unwrap_or_default() {
            "ui" if message["surface"] == "panel" => {
                if self.code.contributes.panel.is_none() {
                    self.problem("drew a panel, which its plugin.json does not declare.");
                } else if let Err(why) = ui::set_panel(&self.name, &message["tree"]) {
                    self.problem(&why);
                }
            }
            "status" => {
                if self.code.contributes.status {
                    ui::set_status(&self.name, text);
                } else {
                    self.problem("set a status, which its plugin.json does not declare.");
                }
            }
            "toast" => {
                let kind = match message["kind"].as_str() {
                    Some("warning") => ui::Kind::Warning,
                    Some("error") => ui::Kind::Error,
                    _ => ui::Kind::Info,
                };
                if !ui::toast(&self.name, text, kind) {
                    self.log(&format!("notice not shown, too many at once: {text}"));
                }
            }
            "log" => self.log(text),
            "error" => self.problem(text),
            "ready" => {}
            other => self.problem(&format!(
                "sent \"{}\", which Letronna does not know.",
                crate::skills::printable(other)
            )),
        }
    }

    /// `rest` said of this plugin, in a sentence.
    pub(super) fn said(&self, rest: &str) -> String {
        format!("The {} plugin {rest}", self.name)
    }

    /// Tells the user `rest` of this plugin, unless it has said a lot
    /// lately; always into its log.
    pub(super) fn problem(&self, rest: &str) {
        self.log(rest);
        let mut trouble = lock(&self.trouble);
        let now = Instant::now();
        trouble
            .problems
            .retain(|at| now.duration_since(*at) < PROBLEM_WINDOW);
        if trouble.problems.len() >= PROBLEMS {
            return;
        }
        trouble.problems.push(now);
        drop(trouble);
        crate::hooks::report(vec![self.said(rest)]);
    }

    /// Adds `text` to its log, letting the older half go past `LOG_BYTES`.
    pub(super) fn log(&self, text: &str) {
        let _held = lock(&self.log);
        let file = self.own.join("log.txt");
        if std::fs::metadata(&file).is_ok_and(|meta| meta.len() > LOG_BYTES) {
            let _ = std::fs::rename(&file, self.own.join("log.old.txt"));
        }
        let _ = std::fs::create_dir_all(&self.own);
        if let Ok(mut log) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&file)
        {
            for line in text.lines() {
                let _ = writeln!(log, "{} {line}", crate::chat::now());
            }
        }
    }

    /// The first thing its code hears: who it is, where to keep things, and
    /// its settings.
    pub(super) fn hello(&self) -> Value {
        json!({
            "type": "hello",
            "sdk": 1,
            "plugin": self.name,
            "data": self.own,
            "events": self.code.contributes.events,
            "settings": self.settings(),
        })
    }

    /// Its settings as they are now: what the user chose where it fits the
    /// setting, else the default.
    pub(super) fn settings(&self) -> Map<String, Value> {
        let saved: Map<String, Value> =
            crate::state::read(&self.own.join("settings.json")).unwrap_or_default();
        self.code
            .contributes
            .settings
            .iter()
            .map(|setting| {
                let chosen = saved
                    .get(&setting.key)
                    .filter(|value| fits(setting, value).is_ok())
                    .cloned();
                let value = chosen.unwrap_or_else(|| default(setting));
                (setting.key.clone(), value)
            })
            .collect()
    }

    /// Keeps `value` as the setting `key`, telling the code if it runs.
    pub(super) fn set(&self, key: &str, value: Value) -> Result<(), String> {
        let setting = (self.code.contributes.settings.iter())
            .find(|setting| setting.key == key)
            .ok_or_else(|| self.said(&format!("has no setting called {key}.")))?;
        let value = fits(setting, &value)?;
        std::fs::create_dir_all(&self.own)
            .map_err(|e| format!("Could not make {}: {e}", self.own.display()))?;
        let path = self.own.join("settings.json");
        crate::state::update::<Value>(&path, |saved| {
            if !saved.is_object() {
                *saved = json!({});
            }
            saved[key] = value.clone();
        })?;
        if self.running() {
            self.post(json!({"type": "settings.change", "key": key, "value": value}));
        }
        Ok(())
    }
}

/// `value` as the setting takes it, or why it does not fit.
fn fits(setting: &super::Setting, value: &Value) -> Result<Value, String> {
    let key = &setting.key;
    match setting.kind {
        SettingKind::Switch => value
            .as_bool()
            .map(Value::from)
            .ok_or_else(|| format!("{key} is on or off.")),
        SettingKind::Text => value
            .as_str()
            .map(|text| Value::from(text.chars().take(SETTING_CHARS).collect::<String>()))
            .ok_or_else(|| format!("{key} is text.")),
        SettingKind::Choice => value
            .as_str()
            .filter(|chosen| setting.choices.iter().any(|choice| choice == chosen))
            .map(Value::from)
            .ok_or_else(|| format!("{key} is one of {}.", setting.choices.join(", "))),
    }
}

/// A setting's default, made to fit when the manifest's does not.
fn default(setting: &super::Setting) -> Value {
    fits(setting, &setting.default).unwrap_or_else(|_| match setting.kind {
        SettingKind::Switch => false.into(),
        SettingKind::Text => "".into(),
        SettingKind::Choice => setting.choices.first().cloned().unwrap_or_default().into(),
    })
}

/// A request given to a plugin, waiting for its answer.
pub(super) struct Pending {
    plugin: Arc<Loaded>,
    id: u64,
    answers: Receiver<Answer>,
    /// Ends the wait when the caller's stop is set; unhooked when dropped.
    _waker: Option<Waker>,
}

impl Pending {
    /// Its answer, waited for until `until`. A request the caller gave up
    /// on is called off, so the plugin can stop working on it.
    pub(super) fn wait(self, until: Instant) -> Waited {
        let answer = self
            .answers
            .recv_timeout(until.saturating_duration_since(Instant::now()));
        lock(&self.plugin.waiting).remove(&self.id);
        let cancel = || {
            self.plugin.post(json!({"type": "cancel", "id": self.id}));
        };
        match answer {
            Ok(Answer::Said(said)) => Waited::Said(said),
            Ok(Answer::Failed(why)) => Waited::Failed(why),
            Ok(Answer::Stopped) => {
                cancel();
                Waited::Stopped
            }
            Err(RecvTimeoutError::Timeout | RecvTimeoutError::Disconnected) => {
                cancel();
                Waited::Late
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn code() -> Code {
        Code {
            runtime: super::super::Runtime::JavaScript,
            main: "plugin.js".into(),
            start: super::super::Start::OnUse,
            network: false,
            conversation: false,
            contributes: Default::default(),
        }
    }

    #[test]
    fn a_plugin_that_falls_behind_has_what_it_is_sent_dropped_and_counted() {
        let own = std::env::temp_dir().join(format!("letronna-behind-{}", std::process::id()));
        let plugin = Loaded::new("behind".into(), own.clone(), own.clone(), code());
        for _ in 0..QUEUE {
            assert!(plugin.post(json!({"type": "event"})));
        }
        assert!(!plugin.post(json!({"type": "event"})));
        assert!(!plugin.post(json!({"type": "event"})));
        assert_eq!(plugin.dropped.load(Ordering::Relaxed), 2);
        // Told once, and kept in its log.
        let log = std::fs::read_to_string(own.join("log.txt")).unwrap();
        assert_eq!(log.matches("is behind").count(), 1);
        let _ = std::fs::remove_dir_all(&own);
    }

    #[test]
    fn what_was_queued_before_a_stop_is_passed_over_by_the_next_host() {
        let own = std::env::temp_dir().join(format!("letronna-stale-{}", std::process::id()));
        let plugin = Loaded::new("stale".into(), own.clone(), own.clone(), code());
        plugin.post(json!({"type": "event"}));
        let queued = plugin.queue.lock().unwrap().try_recv().unwrap();
        assert!(!Loaded::stale(&queued, plugin.epoch()));
        plugin.stop();
        assert!(Loaded::stale(&queued, plugin.epoch()));
        // A host's own messages, such as a fetch's answer, are never stale.
        assert!(!Loaded::stale(&json!({"type": "fetched"}), plugin.epoch()));
        let _ = std::fs::remove_dir_all(&own);
    }

    #[test]
    fn a_plugin_that_keeps_failing_waits_longer_each_time_and_is_then_left_off() {
        let own = std::env::temp_dir().join(format!("letronna-failing-{}", std::process::id()));
        let plugin = Loaded::new("failing".into(), own.clone(), own.clone(), code());
        plugin.failed("it broke.");
        let error = plugin.may_start().unwrap_err();
        assert!(error.contains("starts again in"), "{error}");
        for _ in 1..CRASHES {
            plugin.failed("it broke.");
        }
        let error = plugin.may_start().unwrap_err();
        assert!(error.contains("left off until it is reloaded"), "{error}");
        plugin.forgive();
        assert!(plugin.may_start().is_ok());
        plugin.retire();
        assert!(plugin.may_start().unwrap_err().contains("switched off"));
        let _ = std::fs::remove_dir_all(&own);
    }
}
