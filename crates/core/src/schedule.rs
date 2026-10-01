//! Prompts that run on a timer.
//!
//! A schedule is a prompt plus a project folder plus when to run it. When one
//! is due, `Runner` opens a fresh session and runs the prompt there as a
//! driver run, so the result lands in the sessions like any other
//! conversation and goes out to the chat apps set up to hear it. Letronna
//! lives in the tray, so this works with the window hidden.
//!
//! Deliberately not cron: a short enum covers "every so often" and "at this
//! time of day", which is what a desktop agent is actually asked for, and it
//! reads back to the user without a syntax lesson.
//!
//! The window and the terminal both start a runner, and whichever is open
//! runs the schedules -- but only one of them, or a job would run once per
//! Letronna. The one that does holds an exclusive lock on `schedules.lock`
//! in the data folder for as long as it lives. The system lets the lock go
//! when that Letronna quits, crashes or is killed, so there is no age to
//! judge and no takeover to race: another Letronna waits on it with a thread
//! and has it the moment it is free. Every change to `schedules.json` -- an
//! edit, or a run's `last_run` -- is read, made and saved under a second,
//! short lock, so no Letronna saves over another's change.
//!
//! Nothing polls for due jobs. One timer entry waits for the next one due,
//! and the owner looks again every `BEAT`. A job missed while no Letronna
//! was open runs once when one opens, not once for every time it missed.
//! One that fails before the model says anything is tried once more a beat
//! later; one cut off by quitting is not run again, but said to be at the
//! next start.
//!
//! A run's calls are put to the gate in the schedule's own mode, and one
//! that needs a yes goes to `approvals` as a helper's does. The frontend
//! serving those finds the run with `running` and decides in that mode too,
//! never in whatever mode it is in itself: in Default the call waits for the
//! user at a card in the run's session, as a chat's does, and any other mode
//! decides it with nobody asked.

use crate::chat::{self as backend, Connection, Message};
use crate::driver::{self, Allow, Outcome, Spec};
use crate::gate::Mode;
use crate::store::Transcript;
use crate::timers::{self, TimerId};
use crate::{agent, gateway, messaging, store};
use chrono::{DateTime, Datelike, Local, TimeZone, Timelike};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs::{File, TryLockError};
use std::io::{Read, Write};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

/// Schedules running at once, as driver runs. Two lets a quick job run while a
/// slow one is still going, and keeps a morning's backlog after a night
/// asleep to two model runs at a time instead of all of them together.
pub const AT_ONCE: usize = 2;

/// How often the Letronna running the schedules looks again with nothing
/// due. A minute keeps the wake-ups rare while idle, and is also how soon a
/// change made in another Letronna, a clock that jumped over a sleep, or a
/// save that failed is looked at again.
const BEAT: Duration = Duration::from_secs(60);

/// How often a Letronna whose system cannot wait on the lock -- some network
/// drives cannot -- tries it again. Only there does a retry notice the owner
/// gone; five minutes keeps that rare, and what came due meanwhile is caught
/// up when it takes over.
const RETRY: Duration = Duration::from_secs(5 * 60);

#[derive(Clone, Serialize, Deserialize)]
pub enum Trigger {
    /// Every N minutes since the last run.
    Every { minutes: u32 },
    /// At a wall-clock time each day, optionally only Monday to Friday.
    DailyAt {
        hour: u32,
        minute: u32,
        weekdays_only: bool,
    },
    /// A kind this build does not know, from a newer Letronna sharing the
    /// data folder: kept as it was written, and never run here.
    #[serde(untagged)]
    Unknown(serde_json::Value),
}

impl Trigger {
    pub fn label(&self) -> String {
        match self {
            Trigger::Every { minutes } if *minutes % 60 == 0 && *minutes >= 60 => {
                let hours = minutes / 60;
                if hours == 1 {
                    "Every hour".into()
                } else {
                    format!("Every {hours} hours")
                }
            }
            Trigger::Every { minutes } => format!("Every {minutes} minutes"),
            Trigger::DailyAt {
                hour,
                minute,
                weekdays_only,
            } => format!(
                "{} at {hour:02}:{minute:02}",
                if *weekdays_only { "Weekdays" } else { "Daily" }
            ),
            Trigger::Unknown(_) => "Set in a newer Letronna".into(),
        }
    }
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Schedule {
    pub id: u64,
    pub name: String,
    pub prompt: String,
    #[serde(default)]
    pub workspace: Option<PathBuf>,
    pub trigger: Trigger,
    #[serde(default = "yes")]
    pub enabled: bool,
    /// Epoch seconds of the last run; 0 if it has never run.
    #[serde(default)]
    pub last_run: u64,
    /// The mode each run decides in, and the only one: nobody is there to
    /// answer, so what would ask in it is refused. Saves from before the
    /// modes kept `unattended` here, which reads as Bypass when true.
    #[serde(default, alias = "unattended")]
    pub mode: Mode,
}

fn yes() -> bool {
    true
}

/// The most recent moment this schedule should have run at or before `now`,
/// as epoch seconds. `None` when it has never been due.
fn due_at(schedule: &Schedule, now: DateTime<Local>) -> Option<u64> {
    match schedule.trigger {
        Trigger::Every { minutes } => {
            let every = (minutes.max(1) as u64) * 60;
            let now = now.timestamp().max(0) as u64;
            // A schedule that has never run starts its clock now rather than
            // firing immediately on creation.
            if schedule.last_run == 0 {
                return None;
            }
            (now >= schedule.last_run + every).then_some(now)
        }
        Trigger::DailyAt {
            hour,
            minute,
            weekdays_only,
        } => {
            let mut day = now.date_naive();
            let time_today = now
                .with_hour(hour)?
                .with_minute(minute)?
                .with_second(0)?
                .with_nanosecond(0)?;
            if time_today > now {
                day = day.pred_opt()?;
            }
            // Step back to the last day this is allowed to run on.
            for _ in 0..7 {
                let weekday = day.weekday().num_days_from_monday();
                if !weekdays_only || weekday < 5 {
                    break;
                }
                day = day.pred_opt()?;
            }
            let at = day.and_hms_opt(hour, minute, 0)?;
            let at = Local.from_local_datetime(&at).single()?;
            Some(at.timestamp().max(0) as u64)
        }
        Trigger::Unknown(_) => None,
    }
}

/// Whether the schedule should run now, given when it last did.
pub fn is_due(schedule: &Schedule, now: DateTime<Local>) -> bool {
    if !schedule.enabled {
        return false;
    }
    match due_at(schedule, now) {
        Some(at) => schedule.last_run < at || schedule.last_run == 0,
        None => false,
    }
}

/// Start a new or changed schedule's clock now, so it waits for its next
/// time rather than running at once.
pub fn armed_now(schedule: &mut Schedule) {
    if matches!(
        schedule.trigger,
        Trigger::Every { .. } | Trigger::DailyAt { .. }
    ) {
        schedule.last_run = backend::now();
    }
}

/// The earliest moment from `now` on at which the schedule is due, as epoch
/// seconds: `now` itself when it already is, `None` when it never will be.
pub fn next_due(schedule: &Schedule, now: DateTime<Local>) -> Option<u64> {
    if !schedule.enabled {
        return None;
    }
    if is_due(schedule, now) {
        return Some(now.timestamp().max(0) as u64);
    }
    match schedule.trigger {
        Trigger::Every { minutes } if schedule.last_run > 0 => {
            Some(schedule.last_run + (minutes.max(1) as u64) * 60)
        }
        Trigger::Every { .. } => None,
        Trigger::DailyAt {
            hour,
            minute,
            weekdays_only,
        } => {
            // Today's time if it is still to come, else the first allowed day
            // after; a week always holds one.
            let mut day = now.date_naive();
            for _ in 0..8 {
                let allowed = !weekdays_only || day.weekday().num_days_from_monday() < 5;
                let at = day
                    .and_hms_opt(hour, minute, 0)
                    .and_then(|at| Local.from_local_datetime(&at).single());
                if let Some(at) = at.filter(|at| allowed && *at > now) {
                    return Some(at.timestamp().max(0) as u64);
                }
                day = day.succ_opt()?;
            }
            None
        }
        Trigger::Unknown(_) => None,
    }
}

/// Where the schedules are kept in data folder `dir`.
fn path(dir: &Path) -> PathBuf {
    dir.join("schedules.json")
}

/// The schedules on disk: none when there is no file yet, and why not when
/// there is one this build cannot read -- which is then never saved over.
fn load(dir: &Path) -> Result<Vec<Schedule>, String> {
    let bytes = match std::fs::read(path(dir)) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(format!("Could not read the scheduled jobs: {e}")),
    };
    read(&bytes).map_err(|e| {
        format!(
            "The scheduled jobs in {} cannot be read ({e}), so none of them runs \
             and nothing is saved over the file until it is fixed.",
            path(dir).display()
        )
    })
}

/// Schedules from their file's `bytes`, where one saved with both keeps
/// its `mode` over the `unattended` it replaced.
pub(crate) fn read(bytes: &[u8]) -> serde_json::Result<Vec<Schedule>> {
    let mut saved: serde_json::Value = serde_json::from_slice(bytes)?;
    for schedule in saved.as_array_mut().into_iter().flatten() {
        crate::gate::keep_mode(schedule, "unattended");
    }
    serde_json::from_value(saved)
}

/// Write the schedules whole, through a file of this process's own, so a
/// Letronna reading at that moment finds the old list or the new one and
/// never half of one.
fn save(dir: &Path, schedules: &[Schedule]) -> Result<(), String> {
    let failed = |e: std::io::Error| format!("Could not save the scheduled jobs: {e}");
    std::fs::create_dir_all(dir).map_err(failed)?;
    let bytes = serde_json::to_vec_pretty(schedules)
        .map_err(|e| format!("Could not save the scheduled jobs: {e}"))?;
    let temp = path(dir).with_extension(format!("{}.tmp", std::process::id()));
    std::fs::write(&temp, bytes)
        .and_then(|()| std::fs::rename(&temp, path(dir)))
        .map_err(|e| {
            let _ = std::fs::remove_file(&temp);
            failed(e)
        })
}

/// Read the list, let `edit` change it and save what it leaves, all under
/// the list's own lock, so no other Letronna's change lands in between and
/// is lost. An error from `edit` leaves the file as it was.
fn edit_list<T>(
    dir: &Path,
    edit: impl FnOnce(&mut Vec<Schedule>) -> Result<T, String>,
) -> Result<T, String> {
    let _held = lock_list(dir)?;
    let mut list = load(dir)?;
    let out = edit(&mut list)?;
    save(dir, &list)?;
    Ok(out)
}

pub fn next_id(schedules: &[Schedule]) -> u64 {
    schedules.iter().map(|s| s.id).max().unwrap_or(0) + 1
}

/// How a scheduled run ended, for the Letronna that ran it to show.
#[derive(Debug, PartialEq, Eq)]
pub struct Ended {
    pub schedule: u64,
    pub name: String,
    /// The session it ran in, or `None` when it could not start.
    pub session: Option<u64>,
    /// Its answer, or why it did not start or finish.
    pub text: String,
    /// It failed before the model said anything, as when the network is not
    /// back yet after a sleep: nothing was done, so it is tried once more.
    pub failed_early: bool,
}

/// What a runner tells the Letronna it runs in. Row 55 adds a run's start
/// here, with its session, `Status` and `Cancel`, to show and stop it beside
/// the chat.
#[derive(Debug, PartialEq, Eq)]
pub enum Event {
    /// A run here ended, or one a quit cut off was found.
    Ended(Ended),
    /// Why the schedules are not running, said once until it changes.
    Trouble(String),
}

/// Runs one schedule to its end on the thread it is called on.
type Work = Box<dyn Fn(&Schedule) -> Ended + Send + Sync>;

/// Runs the schedules as they come due, while this Letronna holds the lock.
/// A clone is one more handle on the same runner.
///
/// Every change to the list goes through `change`, which reads the file
/// afresh under the list's lock and saves at once: the window's edits now,
/// and the model's `schedule` tool's when it comes, through `current`. What
/// happens comes on the receiver `start` hands back.
#[derive(Clone)]
pub struct Runner(Arc<Shared>);

struct Shared {
    /// The data folder, holding `schedules.json` and its locks.
    dir: PathBuf,
    work: Work,
    events: async_channel::Sender<Event>,
    state: Mutex<State>,
}

#[derive(Default)]
struct State {
    /// The lock, held open while this runner runs the schedules; the system
    /// lets it go if this Letronna dies.
    held: Option<File>,
    /// Whether a thread is waiting for another Letronna's lock to come free.
    waiting: bool,
    /// Schedules running here now, by id.
    running: HashSet<u64>,
    /// Schedules someone here asked to run now, waiting for a free place.
    asked: HashSet<u64>,
    /// Schedules that failed before the model said anything, and the epoch
    /// second of their one more try.
    retry: HashMap<u64, u64>,
    /// What last kept the schedules from running, so it is said once.
    trouble: Option<String>,
    /// The one timer for the next schedule due, and when that is.
    due: Option<(TimerId, u64)>,
    /// The timer for the next look with nothing due: the beat, or a retry.
    beat: Option<TimerId>,
    stopped: bool,
}

/// The runner a frontend started, for `stop` and `current` to find.
static STARTED: Mutex<Option<Runner>> = Mutex::new(None);

/// The runs going on in this Letronna, by the session each runs in.
static RUNNING: Mutex<BTreeMap<u64, Running>> = Mutex::new(BTreeMap::new());

/// A scheduled run going on in this Letronna, as the frontend answering its
/// calls needs it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Running {
    /// The schedule's name.
    pub name: String,
    /// The schedule's mode, which its calls and its helpers' are decided in.
    pub mode: Mode,
    /// The project folder it runs in.
    pub workspace: Option<PathBuf>,
}

impl Running {
    /// The mode it decides in without the user, as `Gate::for_window` takes
    /// it: None in Default, where a call waits for the user as a chat's does.
    pub fn unattended(&self) -> Option<Mode> {
        (self.mode != Mode::Default).then_some(self.mode)
    }
}

/// The scheduled run going on in session `session` here, if there is one. A
/// call from it or from one of its helpers is decided in its schedule's mode,
/// never in the mode of the frontend answering it.
pub fn running(session: u64) -> Option<Running> {
    lock(&RUNNING).get(&session).cloned()
}

/// A run listed in `RUNNING` for as long as this is held, a panic included.
pub(crate) struct Listed(u64);

impl Listed {
    /// Lists `job`'s run in `session`.
    pub(crate) fn new(session: u64, job: &Schedule) -> Self {
        let run = Running {
            name: job.name.clone(),
            mode: job.mode,
            workspace: job.workspace.clone(),
        };
        lock(&RUNNING).insert(session, run);
        Self(session)
    }
}

impl Drop for Listed {
    fn drop(&mut self) {
        lock(&RUNNING).remove(&self.0);
    }
}

impl Runner {
    /// Start running the schedules in the data folder, each in a session of
    /// its own; what happens comes on the receiver.
    pub fn start() -> (Self, async_channel::Receiver<Event>) {
        let (runner, events) = Self::with(backend::data_dir(), Box::new(run_in_session));
        *lock(&STARTED) = Some(runner.clone());
        (runner, events)
    }

    /// A runner on data folder `dir` whose runs are `work`, taking its first look now.
    fn with(dir: PathBuf, work: Work) -> (Self, async_channel::Receiver<Event>) {
        let (sender, events) = async_channel::unbounded();
        let runner = Self(Arc::new(Shared {
            dir,
            work,
            events: sender,
            state: Mutex::default(),
        }));
        report_cut_off(&runner.0);
        step(&runner.0);
        (runner, events)
    }

    /// The schedules as they are on disk now; none while the file cannot be
    /// read, which the receiver has said.
    pub fn list(&self) -> Vec<Schedule> {
        load(&self.0.dir).unwrap_or_default()
    }

    /// Change the list: `edit` gets it fresh from disk, what it leaves is
    /// saved at once, and the next due time is worked out again. Gives what
    /// `edit` gave, or why nothing was saved.
    pub fn change<T>(&self, edit: impl FnOnce(&mut Vec<Schedule>) -> T) -> Result<T, String> {
        let changed = edit_list(&self.0.dir, |list| Ok(edit(list)));
        step(&self.0);
        changed
    }

    /// Run schedule `id` here as soon as fewer than `AT_ONCE` are running,
    /// whatever its time and whichever Letronna holds the lock: somebody
    /// here asked for it.
    pub fn run_now(&self, id: u64) {
        lock(&self.0.state).asked.insert(id);
        step(&self.0);
    }

    /// Start nothing more and give the lock up; runs already going finish,
    /// unheard: a window that moved to another profile would take a session
    /// number of the old one's for its own.
    fn stop(&self) {
        let mut state = lock(&self.0.state);
        state.stopped = true;
        let due = state.due.take().map(|(timer, _)| timer);
        for timer in due.into_iter().chain(state.beat.take()) {
            timers::cancel(timer);
        }
        // Closing it lets another Letronna's waiting thread have it at once.
        state.held = None;
        self.0.events.close();
    }
}

/// Stop the runner a frontend started, so another Letronna takes the
/// schedules over at once.
pub fn stop() {
    if let Some(runner) = lock(&STARTED).take() {
        runner.stop();
    }
}

/// The runner this Letronna started, for a tool to change the schedules
/// through; `None` before a frontend starts one, as under `-p`.
pub fn current() -> Option<Runner> {
    lock(&STARTED).clone()
}

/// Take the lock if it is free, start what is due while there is room, and
/// set the timers for the next look.
fn step(shared: &Arc<Shared>) {
    let mut state = lock(&shared.state);
    if state.stopped {
        return;
    }
    let now = Local::now();
    let looked = take_lock(shared, &mut state).and_then(|()| start_due(shared, &mut state, now));
    let list = match looked {
        Ok((list, starting)) => {
            state.trouble = None;
            for (job, previous) in starting {
                launch(shared, &mut state, job, previous);
            }
            list
        }
        Err(trouble) => {
            if state.trouble.as_ref() != Some(&trouble) {
                let _ = shared.events.try_send(Event::Trouble(trouble.clone()));
                state.trouble = Some(trouble);
            }
            // Nothing is armed for what is due, or a failed save would be
            // tried again at once, and again: the beat looks next.
            Vec::new()
        }
    };
    // The owner looks again every beat. A thread waits on another
    // Letronna's lock and looks the moment it is free; only where the
    // system cannot wait on a lock is it tried again now and then.
    let again = match (state.held.is_some(), state.waiting) {
        (true, _) => Some(BEAT),
        (false, true) => None,
        (false, false) => Some(RETRY),
    };
    let beat = again.map(|wait| later(shared, wait));
    if let Some(old) = std::mem::replace(&mut state.beat, beat) {
        timers::cancel(old);
    }
    // With every place taken, the next run to end looks again instead.
    let next = if state.held.is_some() && state.running.len() < AT_ONCE {
        list.iter()
            .filter(|job| !state.running.contains(&job.id))
            .filter_map(|job| {
                let at = next_due(job, now)?;
                Some(state.retry.get(&job.id).map_or(at, |&retry| at.max(retry)))
            })
            .min()
    } else {
        None
    };
    if let Some((timer, _)) = state.due.take() {
        timers::cancel(timer);
    }
    state.due = next.map(|at| {
        let from = now.timestamp_millis().max(0) as u64;
        let wait = Duration::from_millis((at * 1000).saturating_sub(from));
        (later(shared, wait), at)
    });
}

/// A job about to start, with the `last_run` it had until now.
type Starting = (Schedule, u64);

/// Under the list's lock, pick what may start now and save each one's
/// `last_run` before any of them starts, so no look from here or elsewhere
/// finds it still due. Gives the list and the jobs to start, each with the
/// `last_run` it had, or why nothing could start.
fn start_due(
    shared: &Shared,
    state: &mut State,
    now: DateTime<Local>,
) -> Result<(Vec<Schedule>, Vec<Starting>), String> {
    let _held = lock_list(&shared.dir)?;
    // Read each time: another Letronna may have changed it since.
    let mut list = load(&shared.dir)?;
    // A job deleted since it was asked for would hold its ask for ever.
    state
        .asked
        .retain(|id| list.iter().any(|job| job.id == *id));
    state
        .retry
        .retain(|id, _| list.iter().any(|job| job.id == *id));
    let owner = state.held.is_some();
    let second = now.timestamp().max(0) as u64;
    let starting: Vec<usize> = (0..list.len())
        .filter(|&ix| {
            let job = &list[ix];
            let waits = state.retry.get(&job.id).is_some_and(|&at| at > second);
            !state.running.contains(&job.id)
                && !matches!(job.trigger, Trigger::Unknown(_))
                && (state.asked.contains(&job.id) || (owner && !waits && is_due(job, now)))
        })
        .take(AT_ONCE.saturating_sub(state.running.len()))
        .collect();
    if starting.is_empty() {
        return Ok((list, Vec::new()));
    }
    let previous: Vec<u64> = starting.iter().map(|&ix| list[ix].last_run).collect();
    for &ix in &starting {
        list[ix].last_run = backend::now();
    }
    save(&shared.dir, &list)
        .map_err(|e| format!("{e}. Nothing due was run; it is tried again in a minute."))?;
    let starting = starting
        .into_iter()
        .zip(previous)
        .map(|(ix, previous)| (list[ix].clone(), previous))
        .collect();
    Ok((list, starting))
}

/// Run `job` on a thread of its own; when it ends, pass that on and look
/// again, since a place is free. `previous` is the `last_run` it had, given
/// back once if the run fails before the model says anything.
fn launch(shared: &Arc<Shared>, state: &mut State, job: Schedule, previous: u64) {
    let id = job.id;
    state.asked.remove(&id);
    let second_try = state.retry.remove(&id).is_some();
    state.running.insert(id);
    let shared = Arc::clone(shared);
    let spawned = std::thread::Builder::new()
        .name(format!("schedule-{id}"))
        .spawn(move || {
            let marker = mark(&shared.dir, &job);
            // A run that panicked must still give its place back.
            let mut ended =
                catch_unwind(AssertUnwindSafe(|| (shared.work)(&job))).unwrap_or_else(|_| Ended {
                    schedule: id,
                    name: job.name.clone(),
                    session: None,
                    text: "Did not finish: it crashed.".into(),
                    failed_early: false,
                });
            if let Some((path, _file)) = marker {
                // Before the lock goes, so a Letronna that finds the marker
                // unlocked knows the run did not finish.
                let _ = std::fs::remove_file(path);
            }
            let again = ended.failed_early && !second_try;
            if again {
                give_back(&shared.dir, &job, previous);
                ended.text.push_str(" It is tried once more in a minute.");
            }
            {
                let mut state = lock(&shared.state);
                state.running.remove(&id);
                if again {
                    state.retry.insert(id, backend::now() + BEAT.as_secs());
                }
            }
            let _ = shared.events.try_send(Event::Ended(ended));
            step(&shared);
        });
    if spawned.is_err() {
        state.running.remove(&id);
    }
}

/// Put `job`'s `last_run` back to `previous`, unless something has moved it
/// since its run set it.
fn give_back(dir: &Path, job: &Schedule, previous: u64) {
    let _ = edit_list(dir, |list| {
        if let Some(found) = list
            .iter_mut()
            .find(|found| found.id == job.id && found.last_run == job.last_run)
        {
            found.last_run = previous;
        }
        Ok(())
    });
}

/// A timer that looks again after `wait`. The look reads and writes files,
/// and a timer's callback must be quick, so it gets a thread of its own --
/// or, when there is none to be had, looks right there rather than never.
fn later(shared: &Arc<Shared>, wait: Duration) -> TimerId {
    let shared = Arc::downgrade(shared);
    timers::at(
        Instant::now() + wait,
        Box::new(move || {
            let Some(shared) = shared.upgrade() else {
                return;
            };
            let looking = Arc::clone(&shared);
            if std::thread::Builder::new()
                .name("schedules".into())
                .spawn(move || step(&looking))
                .is_err()
            {
                step(&shared);
            }
        }),
    )
}

/// Open (or make) the lock file at `path`. std opens it close-on-exec, so a
/// command this Letronna started does not hold the lock after it quits.
fn open_lock(path: &Path) -> std::io::Result<File> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    File::options()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)
}

/// Hold the lock that says this runner runs the schedules, if it is free;
/// while another Letronna holds it, have a thread wait for it. An error is
/// only a lock the system will not give anyone.
fn take_lock(shared: &Arc<Shared>, state: &mut State) -> Result<(), String> {
    if state.held.is_some() || state.waiting {
        return Ok(());
    }
    let refused = |e: std::io::Error| format!("Scheduled jobs cannot run: {e}");
    let file = open_lock(&shared.dir.join("schedules.lock")).map_err(refused)?;
    match file.try_lock() {
        Ok(()) => state.held = Some(file),
        Err(TryLockError::WouldBlock) => state.waiting = wait_for_lock(shared, file),
        Err(TryLockError::Error(e)) => return Err(refused(e)),
    }
    Ok(())
}

/// Wait on a thread for the lock in `file`, and take it and look when it
/// comes free. Whether the thread started.
fn wait_for_lock(shared: &Arc<Shared>, file: File) -> bool {
    let weak = Arc::downgrade(shared);
    std::thread::Builder::new()
        .name("schedules-lock".into())
        .spawn(move || {
            let taken = file.lock();
            // A runner gone meanwhile lets the lock go again with the file.
            let Some(shared) = weak.upgrade() else {
                return;
            };
            let mut state = lock(&shared.state);
            state.waiting = false;
            match taken {
                Ok(()) if !state.stopped => state.held = Some(file),
                Ok(()) => return,
                // Try again later rather than straight away, which would
                // fail the same way at once.
                Err(_) => {
                    if let Some(old) = state.beat.replace(later(&shared, RETRY)) {
                        timers::cancel(old);
                    }
                    return;
                }
            }
            drop(state);
            step(&shared);
        })
        .is_ok()
}

/// Hold the list's own lock until the file goes, waiting while another
/// Letronna reads or changes the list, which takes a moment.
fn lock_list(dir: &Path) -> Result<File, String> {
    let refused = |e: std::io::Error| format!("Could not lock the scheduled jobs: {e}");
    let file = open_lock(&dir.join("schedules.json.lock")).map_err(refused)?;
    file.lock().map_err(refused)?;
    Ok(file)
}

/// Where each run in progress keeps its marker.
fn runs_dir(dir: &Path) -> PathBuf {
    dir.join("schedule-runs")
}

/// What a run's marker says, for the Letronna that finds it cut off.
#[derive(Serialize, Deserialize)]
struct Marker {
    schedule: u64,
    name: String,
}

/// Leave a marker for `job`'s run, held locked while the run goes on. It is
/// written and locked under a name no look reads, then renamed, so a marker
/// that can be seen is always whole and locked.
fn mark(dir: &Path, job: &Schedule) -> Option<(PathBuf, File)> {
    static RUNS: AtomicU64 = AtomicU64::new(0);
    let runs = runs_dir(dir);
    std::fs::create_dir_all(&runs).ok()?;
    let run = format!(
        "{}-{}-{}",
        std::process::id(),
        RUNS.fetch_add(1, Ordering::Relaxed),
        backend::now()
    );
    let temp = runs.join(format!("{run}.tmp"));
    let path = runs.join(format!("{run}.json"));
    let marker = Marker {
        schedule: job.id,
        name: job.name.clone(),
    };
    match write_marker(&temp, &path, &marker) {
        Ok(file) => Some((path, file)),
        Err(_) => {
            let _ = std::fs::remove_file(&temp);
            None
        }
    }
}

fn write_marker(
    temp: &Path,
    path: &Path,
    marker: &Marker,
) -> Result<File, Box<dyn std::error::Error>> {
    let mut file = File::options().write(true).create_new(true).open(temp)?;
    file.try_lock()?;
    file.write_all(&serde_json::to_vec(marker)?)?;
    std::fs::rename(temp, path)?;
    Ok(file)
}

/// Say which runs were cut off by a Letronna that quit or died while they
/// ran, and clear their markers: those nobody holds locked any more. They
/// are not run again, which could repeat what they had already done.
fn report_cut_off(shared: &Shared) {
    let Ok(entries) = std::fs::read_dir(runs_dir(&shared.dir)) else {
        return;
    };
    for entry in entries.filter_map(Result::ok) {
        let path = entry.path();
        if path.extension().is_none_or(|ext| ext != "json") {
            continue;
        }
        let Ok(mut file) = File::open(&path) else {
            continue;
        };
        if file.try_lock().is_err() {
            continue;
        }
        // Gone by now means its run finished as this looked.
        if !path.exists() {
            continue;
        }
        // Through the handle that holds the lock: Windows keeps a locked
        // file's bytes from every other handle, this process's included.
        let mut bytes = Vec::new();
        if file.read_to_end(&mut bytes).is_err() {
            continue;
        }
        let _ = std::fs::remove_file(&path);
        let Ok(marker) = serde_json::from_slice::<Marker>(&bytes) else {
            continue;
        };
        let _ = shared.events.try_send(Event::Ended(Ended {
            schedule: marker.schedule,
            name: marker.name,
            session: None,
            text: "Cut off: Letronna quit before it finished, and it was not run again.".into(),
            failed_early: false,
        }));
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    // The state changes a field at a time, so a panic under the lock leaves
    // it whole.
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Runs `job` in a session of its own as a driver run, then tells the chat
/// apps how it went, as the window did when it ran schedules itself.
fn run_in_session(job: &Schedule) -> Ended {
    let (session, text, failed_early) = match open(job) {
        Ok(spec) => {
            let session = spec.root;
            // Before it starts, so its first call is decided in its mode.
            let _listed = Listed::new(session, job);
            let report = driver::run(spec);
            let early = matches!(report.status, Outcome::Failed(_))
                && report.rounds == 0
                && report.text.trim().is_empty();
            (Some(session), said(&report), early)
        }
        Err(error) => (None, format!("Did not start: {error}"), true),
    };
    let _ = messaging::report(&messaging::load(), &job.name, &text);
    Ended {
        schedule: job.id,
        name: job.name.clone(),
        session,
        text,
        failed_early,
    }
}

/// A run of `job` in a new session, on the gateway and model set up now.
fn open(job: &Schedule) -> Result<Spec, String> {
    let settings = store::current_settings();
    let saved = store::load_keys()
        .get(gateway::key_slot(&settings.gateway))
        .cloned()
        .unwrap_or_default();
    let (endpoint, key) = gateway::resolve(&settings.gateway, &settings.custom_endpoint, &saved);
    let connection = Connection {
        endpoint,
        model: settings.model.clone(),
        key,
        tools: true,
        workspace: job.workspace.clone(),
        effort: settings.effort.clone(),
        search: settings.search,
        ..Default::default()
    };
    // Before the session is made, so a run that cannot start leaves none.
    backend::validate(&connection)?;
    let transcript = Transcript::session(&job.name, job.workspace.clone())?;
    Ok(run_of(job, connection, transcript))
}

/// The run of `job` as the session `transcript` begins, on `connection`: in
/// the job's own mode with nobody to answer, so whatever would ask is
/// refused, and in Auto whatever the judge does not allow.
pub(crate) fn run_of(job: &Schedule, connection: Connection, mut transcript: Transcript) -> Spec {
    let id = transcript.conversation.id;
    transcript.conversation.messages.push(Message {
        role: "user".into(),
        content: job.prompt.clone(),
        ..Default::default()
    });
    let connection = Connection {
        conversation: id,
        // Never the Default mode setting nor a session's chip: those say
        // what someone watching lets through, and nobody watches this.
        mode: job.mode,
        plan: job.mode == Mode::Plan,
        human: false,
        // Nobody may be watching, so a failed call is tried more times.
        unattended: true,
        // What Auto mode's judge reads as the user's wishes; it judges on
        // the model the run is on.
        typed: [job.prompt.clone()].into(),
        ..connection
    };
    Spec {
        connection,
        transcript,
        id,
        owner: id,
        root: id,
        run: id,
        label: job.name.clone(),
        allow: Allow::All,
        rounds: agent::MAX_ROUNDS,
        cancel: Default::default(),
        budget: None,
        status: Default::default(),
        depth: 0,
        fork: false,
        from: 0,
        schema: None,
        watch: None,
        // The driver puts every call through the whole gate in the run's
        // mode, and with nobody to answer no call reaches a frontend,
        // whose answer would come from whatever mode it is in.
        decide: None,
    }
}

/// What a finished run came to: its answer, or why there is none.
fn said(report: &driver::Report) -> String {
    match &report.status {
        Outcome::Failed(error) => format!("Did not finish: {error}"),
        _ if report.text.trim().is_empty() => "That did not produce an answer.".into(),
        _ => report.text.clone(),
    }
}

#[cfg(test)]
mod tests {
    // Not `use super::*`: the prelude re-exports gpui, whose `test` macro
    // would shadow the built-in attribute here.
    use super::{
        AT_ONCE, Ended, Event, Runner, Schedule, Trigger, armed_now, is_due, load, lock, next_due,
        path, runs_dir, save, step,
    };
    use chrono::{DateTime, Local, TimeZone};
    use std::path::{Path, PathBuf};
    use std::sync::mpsc::{self, Receiver, Sender};
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};

    /// Far longer than a run takes to start, so only a hang fails a test.
    const WAIT: Duration = Duration::from_secs(10);
    /// Long enough for a run that was going to start to have started.
    const QUIET: Duration = Duration::from_millis(300);

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("letronna-schedule-{name}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn now() -> u64 {
        crate::chat::now()
    }

    /// Job `id`, every `minutes`, last run `ago` seconds back.
    fn every(id: u64, minutes: u32, ago: u64) -> Schedule {
        Schedule {
            id,
            last_run: now() - ago,
            ..schedule(Trigger::Every { minutes })
        }
    }

    /// A runner and what it says: which job each run is on as it starts,
    /// the word that ends a run, and the runner's own events.
    struct Probe {
        runner: Runner,
        started: Receiver<u64>,
        finish: Sender<()>,
        events: async_channel::Receiver<Event>,
    }

    impl Probe {
        fn owner(&self) -> bool {
            lock(&self.runner.0.state).held.is_some()
        }

        /// The troubles said so far.
        fn troubles(&self) -> Vec<String> {
            std::iter::from_fn(|| self.events.try_recv().ok())
                .filter_map(|event| match event {
                    Event::Trouble(trouble) => Some(trouble),
                    Event::Ended(_) => None,
                })
                .collect()
        }
    }

    /// A runner on `dir` whose runs end only when the probe's sender has a
    /// word for them.
    fn probe(dir: &Path) -> Probe {
        probe_failing(dir, false)
    }

    /// As `probe`, with each run saying whether it failed before the model
    /// said anything.
    fn probe_failing(dir: &Path, failed_early: bool) -> Probe {
        let (started, heard) = mpsc::channel();
        let (finish, finished) = mpsc::channel::<()>();
        let (started, finished) = (Mutex::new(started), Arc::new(Mutex::new(finished)));
        let work = Box::new(move |job: &Schedule| {
            let _ = started.lock().unwrap().send(job.id);
            let _ = finished.lock().unwrap().recv();
            Ended {
                schedule: job.id,
                name: job.name.clone(),
                session: None,
                text: "done".into(),
                failed_early,
            }
        });
        let (runner, events) = Runner::with(dir.to_owned(), work);
        Probe {
            runner,
            started: heard,
            finish,
            events,
        }
    }

    /// Wait until `done` holds, failing after `WAIT`.
    fn until(what: &str, done: impl Fn() -> bool) {
        let give_up = Instant::now() + WAIT;
        while !done() {
            assert!(Instant::now() < give_up, "{what} never happened");
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    fn at(text: &str) -> DateTime<Local> {
        let naive = chrono::NaiveDateTime::parse_from_str(text, "%Y-%m-%d %H:%M:%S").unwrap();
        Local.from_local_datetime(&naive).unwrap()
    }

    fn schedule(trigger: Trigger) -> Schedule {
        Schedule {
            id: 1,
            name: "test".into(),
            prompt: "do the thing".into(),
            workspace: None,
            trigger,
            enabled: true,
            last_run: 0,
            mode: Default::default(),
        }
    }

    #[test]
    fn interval_waits_for_the_interval() {
        let mut s = schedule(Trigger::Every { minutes: 30 });
        let now = at("2026-09-21 12:00:00");
        // Never run: the clock starts when it is created, not at once.
        assert!(!is_due(&s, now));
        armed_now(&mut s);

        s.last_run = now.timestamp() as u64;
        assert!(!is_due(&s, now));
        assert!(!is_due(&s, at("2026-09-21 12:29:00")));
        assert!(is_due(&s, at("2026-09-21 12:30:00")));

        s.enabled = false;
        assert!(!is_due(&s, at("2026-09-21 13:00:00")));
    }

    #[test]
    fn daily_fires_once_a_day_and_catches_up() {
        let mut s = schedule(Trigger::DailyAt {
            hour: 9,
            minute: 0,
            weekdays_only: false,
        });
        // Monday, before the time: the last due moment was Sunday 09:00.
        assert!(is_due(&s, at("2026-09-21 08:00:00")));

        // Having run at Monday 09:00, it does not run again that day.
        s.last_run = at("2026-09-21 09:00:00").timestamp() as u64;
        assert!(!is_due(&s, at("2026-09-21 09:30:00")));
        assert!(!is_due(&s, at("2026-09-21 23:59:00")));
        // ... but does the next morning.
        assert!(is_due(&s, at("2026-09-22 09:00:00")));
    }

    #[test]
    fn weekdays_only_skips_the_weekend() {
        let mut s = schedule(Trigger::DailyAt {
            hour: 9,
            minute: 0,
            weekdays_only: true,
        });
        // Friday's run happened; Saturday and Sunday must stay quiet.
        s.last_run = at("2026-09-18 09:00:00").timestamp() as u64;
        assert!(!is_due(&s, at("2026-09-19 10:00:00")), "Saturday");
        assert!(!is_due(&s, at("2026-09-20 10:00:00")), "Sunday");
        assert!(is_due(&s, at("2026-09-21 09:00:00")), "Monday");
    }

    #[test]
    fn labels_read_back_plainly() {
        assert_eq!(Trigger::Every { minutes: 30 }.label(), "Every 30 minutes");
        assert_eq!(Trigger::Every { minutes: 60 }.label(), "Every hour");
        assert_eq!(Trigger::Every { minutes: 180 }.label(), "Every 3 hours");
        assert_eq!(
            Trigger::DailyAt {
                hour: 9,
                minute: 5,
                weekdays_only: true
            }
            .label(),
            "Weekdays at 09:05"
        );
    }

    #[test]
    fn the_next_due_time_is_the_soonest_moment_a_job_comes_round() {
        let mut s = schedule(Trigger::Every { minutes: 30 });
        let noon = at("2026-09-21 12:00:00");
        // Never run and never armed: it has no clock to count from.
        assert_eq!(next_due(&s, noon), None);
        s.last_run = at("2026-09-21 11:50:00").timestamp() as u64;
        assert_eq!(
            next_due(&s, noon),
            Some(at("2026-09-21 12:20:00").timestamp() as u64)
        );
        // Due already: now.
        let late = at("2026-09-21 12:45:00");
        assert_eq!(next_due(&s, late), Some(late.timestamp() as u64));
        s.enabled = false;
        assert_eq!(next_due(&s, late), None);

        let mut s = schedule(Trigger::DailyAt {
            hour: 9,
            minute: 0,
            weekdays_only: true,
        });
        // Friday's run is done: the next is Monday, not the weekend.
        s.last_run = at("2026-09-18 09:00:00").timestamp() as u64;
        assert_eq!(
            next_due(&s, at("2026-09-18 10:00:00")),
            Some(at("2026-09-21 09:00:00").timestamp() as u64)
        );
        // Monday before the time: later that morning.
        s.last_run = at("2026-09-21 08:00:00").timestamp() as u64;
        assert_eq!(
            next_due(&s, at("2026-09-21 08:30:00")),
            Some(at("2026-09-21 09:00:00").timestamp() as u64)
        );
    }

    #[test]
    fn a_new_daily_job_waits_for_its_next_time_rather_than_running_at_once() {
        let mut s = schedule(Trigger::DailyAt {
            hour: 0,
            minute: 0,
            weekdays_only: false,
        });
        assert!(is_due(&s, Local::now()), "never run, it counts as missed");
        armed_now(&mut s);
        assert!(!is_due(&s, Local::now()));
        assert!(next_due(&s, Local::now()).is_some_and(|at| at > now()));
    }

    #[test]
    fn one_timer_waits_for_the_soonest_job_and_nothing_runs_before() {
        let dir = scratch("one-timer");
        let soon = every(1, 30, 10 * 60);
        let later = every(2, 60, 0);
        let off = Schedule {
            id: 3,
            enabled: false,
            ..every(3, 1, 60 * 60)
        };
        save(&dir, &[later.clone(), soon.clone(), off]).unwrap();
        let probe = probe(&dir);
        assert!(probe.started.recv_timeout(QUIET).is_err());
        assert!(probe.owner());
        let state = lock(&probe.runner.0.state);
        assert_eq!(state.due.map(|(_, at)| at), Some(soon.last_run + 30 * 60));
        assert!(state.beat.is_some());
    }

    #[test]
    fn at_most_two_jobs_run_at_once_and_the_next_starts_when_one_ends() {
        let dir = scratch("at-once");
        save(
            &dir,
            &[every(1, 1, 120), every(2, 1, 120), every(3, 1, 120)],
        )
        .unwrap();
        let probe = probe(&dir);
        let mut first = vec![
            probe.started.recv_timeout(WAIT).unwrap(),
            probe.started.recv_timeout(WAIT).unwrap(),
        ];
        first.sort();
        assert_eq!(first, [1, 2]);
        assert!(
            probe.started.recv_timeout(QUIET).is_err(),
            "a third started"
        );
        {
            let state = lock(&probe.runner.0.state);
            assert_eq!(state.running.len(), AT_ONCE);
            // Every place is taken, so no timer: the next run to end looks.
            assert!(state.due.is_none());
        }
        // The third is still due, since it has not started.
        assert!(is_due(&load(&dir).unwrap()[2], Local::now()));
        probe.finish.send(()).unwrap();
        assert_eq!(probe.started.recv_timeout(WAIT).unwrap(), 3);
        probe.finish.send(()).unwrap();
        probe.finish.send(()).unwrap();
    }

    #[test]
    fn the_lock_goes_to_one_runner_and_to_another_the_moment_it_is_let_go() {
        let dir = scratch("lock");
        let first = probe(&dir);
        let second = probe(&dir);
        let third = probe(&dir);
        assert!(first.owner());
        assert!(!second.owner() && !third.owner());

        // Dropped, as a killed Letronna's files are closed: no look is asked
        // for, and one of the others has it at once.
        drop(first);
        until("a takeover", || second.owner() || third.owner());
        std::thread::sleep(QUIET);
        assert!(second.owner() != third.owner(), "two owners");

        // Stopped, as on quitting: the last one takes it, and a stopped
        // runner never comes back.
        let (gone, last) = if second.owner() {
            (second, third)
        } else {
            (third, second)
        };
        gone.runner.stop();
        until("the second takeover", || last.owner());
        step(&gone.runner.0);
        assert!(!gone.owner(), "a stopped runner came back");
    }

    #[test]
    fn a_job_missed_while_nothing_was_open_runs_once_when_one_opens() {
        let dir = scratch("catch-up");
        let days = 3 * 24 * 60 * 60;
        let daily = Schedule {
            id: 2,
            last_run: now() - days,
            ..schedule(Trigger::DailyAt {
                hour: 0,
                minute: 0,
                weekdays_only: false,
            })
        };
        save(&dir, &[every(1, 60, days), daily]).unwrap();
        let probe = probe(&dir);
        let mut ran = vec![
            probe.started.recv_timeout(WAIT).unwrap(),
            probe.started.recv_timeout(WAIT).unwrap(),
        ];
        ran.sort();
        assert_eq!(ran, [1, 2]);
        probe.finish.send(()).unwrap();
        probe.finish.send(()).unwrap();
        // Once for all the times missed: the next is a whole interval on.
        assert!(probe.started.recv_timeout(QUIET).is_err(), "it ran twice");
        let list = load(&dir).unwrap();
        assert!(list.iter().all(|job| job.last_run + 60 >= now()));
        let state = lock(&probe.runner.0.state);
        let hourly = list[0].last_run + 60 * 60;
        assert!(state.due.is_some_and(|(_, at)| at > now() && at <= hourly));
    }

    #[test]
    fn run_now_runs_a_job_that_is_not_due_in_whichever_letronna_asks() {
        let dir = scratch("run-now");
        save(&dir, &[every(1, 60, 30)]).unwrap();
        let owner = probe(&dir);
        let other = probe(&dir);
        assert!(owner.started.recv_timeout(QUIET).is_err());
        let before = load(&dir).unwrap()[0].last_run;

        owner.finish.send(()).unwrap();
        owner.runner.run_now(1);
        assert_eq!(owner.started.recv_timeout(WAIT).unwrap(), 1);
        assert!(load(&dir).unwrap()[0].last_run > before);

        // Not the owner, yet asked here: it runs here, and only here.
        assert!(!other.owner());
        other.finish.send(()).unwrap();
        other.runner.run_now(1);
        assert_eq!(other.started.recv_timeout(WAIT).unwrap(), 1);
        assert!(owner.started.recv_timeout(QUIET).is_err());
    }

    #[test]
    fn two_letronnas_on_one_data_folder_run_a_due_job_once() {
        let dir = scratch("two");
        save(&dir, &[every(1, 1, 120)]).unwrap();
        // Both start at the same moment, as two processes might, each with
        // a file of its own on the lock.
        let (a, b) = std::thread::scope(|scope| {
            let a = scope.spawn(|| probe(&dir));
            let b = scope.spawn(|| probe(&dir));
            (a.join().unwrap(), b.join().unwrap())
        });
        assert!(a.owner() != b.owner(), "not exactly one owner");
        let ran = a.started.recv_timeout(QUIET).is_ok() as u32
            + b.started.recv_timeout(QUIET).is_ok() as u32;
        assert_eq!(ran, 1, "the job ran {ran} times");
    }

    #[test]
    fn a_failing_save_runs_nothing_says_so_once_and_tries_again_at_the_next_look() {
        let dir = scratch("failing-save");
        save(&dir, &[every(1, 1, 120)]).unwrap();
        // Where the save writes first, taken, as a full disk would refuse it.
        let blocked = path(&dir).with_extension(format!("{}.tmp", std::process::id()));
        std::fs::create_dir_all(&blocked).unwrap();
        let probe = probe(&dir);
        assert!(probe.started.recv_timeout(QUIET).is_err(), "it ran unsaved");
        for _ in 0..5 {
            step(&probe.runner.0);
        }
        assert!(probe.started.recv_timeout(QUIET).is_err(), "it ran unsaved");
        let troubles = probe.troubles();
        assert_eq!(troubles.len(), 1, "{troubles:?}");
        assert!(troubles[0].starts_with("Could not save the scheduled jobs"));
        assert!(lock(&probe.runner.0.state).due.is_none(), "armed to loop");

        std::fs::remove_dir_all(&blocked).unwrap();
        step(&probe.runner.0);
        assert_eq!(probe.started.recv_timeout(WAIT).unwrap(), 1);
        probe.finish.send(()).unwrap();
        assert!(probe.started.recv_timeout(QUIET).is_err(), "it ran twice");
    }

    #[test]
    fn a_list_that_cannot_be_read_runs_nothing_and_is_never_saved_over() {
        let dir = scratch("unreadable");
        let broken = br#"[{"id": 1, "name": "typo" "#;
        std::fs::write(path(&dir), broken).unwrap();
        let probe = probe(&dir);
        step(&probe.runner.0);
        let troubles = probe.troubles();
        assert_eq!(troubles.len(), 1, "{troubles:?}");
        assert!(troubles[0].contains("cannot be read"), "{}", troubles[0]);

        let changed = probe.runner.change(|list| list.push(every(2, 60, 0)));
        assert!(changed.is_err());
        assert_eq!(std::fs::read(path(&dir)).unwrap(), broken);
        assert!(probe.runner.list().is_empty());
    }

    #[test]
    fn a_trigger_from_a_newer_letronna_is_kept_as_it_was_and_never_run() {
        let dir = scratch("newer");
        let once = serde_json::json!({"Once": {"at": 1, "here": true}});
        let mut newer = every(2, 1, 120);
        newer.trigger = serde_json::from_value(once.clone()).unwrap();
        save(&dir, &[every(1, 60, 0), newer]).unwrap();
        let probe = probe(&dir);
        probe.runner.run_now(2);
        assert!(probe.started.recv_timeout(QUIET).is_err(), "it ran");
        assert_eq!(probe.runner.list().len(), 2);

        probe
            .runner
            .change(|list| list.push(every(3, 60, 0)))
            .unwrap();
        let saved: serde_json::Value =
            serde_json::from_slice(&std::fs::read(path(&dir)).unwrap()).unwrap();
        assert_eq!(saved[1]["trigger"], once);
        assert_eq!(
            probe.runner.list()[1].trigger.label(),
            "Set in a newer Letronna"
        );
    }

    #[test]
    fn a_change_in_one_letronna_and_a_run_starting_in_another_both_stand() {
        let dir = scratch("change-race");
        let off = Schedule {
            enabled: false,
            ..every(1, 1, 120)
        };
        save(&dir, &[off]).unwrap();
        let owner = probe(&dir);
        let other = probe(&dir);
        assert!(owner.owner() && !other.owner());

        let looked = other
            .runner
            .change(|list| {
                list[0].enabled = true;
                // The owner looks while this change is part way through; it
                // has to wait for it rather than save in between.
                let looking = owner.runner.clone();
                let look = std::thread::spawn(move || step(&looking.0));
                assert!(
                    owner.started.recv_timeout(QUIET).is_err(),
                    "looked in between"
                );
                list.push(every(2, 60, 0));
                look
            })
            .unwrap();
        assert_eq!(owner.started.recv_timeout(WAIT).unwrap(), 1);
        looked.join().unwrap();
        let ran = load(&dir).unwrap()[0].last_run;
        assert!(ran + 60 >= now());

        // A later change reads the owner's `last_run` rather than undoing it.
        other
            .runner
            .change(|list| list[1].name = "renamed".into())
            .unwrap();
        owner.finish.send(()).unwrap();
        assert!(owner.started.recv_timeout(QUIET).is_err(), "it ran twice");
        let list = load(&dir).unwrap();
        assert_eq!((list[0].last_run, list[1].name.as_str()), (ran, "renamed"));
    }

    #[test]
    fn a_run_that_fails_before_the_model_answers_is_tried_once_more_a_beat_later() {
        let dir = scratch("failed-early");
        let before = every(1, 60, 2 * 60 * 60);
        save(&dir, std::slice::from_ref(&before)).unwrap();
        let probe = probe_failing(&dir, true);
        assert_eq!(probe.started.recv_timeout(WAIT).unwrap(), 1);
        probe.finish.send(()).unwrap();
        // Its time is given back, but it waits for the beat.
        until("the time given back", || {
            load(&dir).unwrap()[0].last_run == before.last_run
        });
        assert!(probe.started.recv_timeout(QUIET).is_err(), "tried at once");
        let retry = lock(&probe.runner.0.state).retry.get(&1).copied();
        assert!(retry.is_some_and(|at| at >= now() + 50), "{retry:?}");
        assert!(
            lock(&probe.runner.0.state)
                .due
                .is_some_and(|(_, at)| Some(at) == retry)
        );

        // The beat comes; the second try fails the same way and keeps its time.
        lock(&probe.runner.0.state).retry.insert(1, now());
        step(&probe.runner.0);
        assert_eq!(probe.started.recv_timeout(WAIT).unwrap(), 1);
        probe.finish.send(()).unwrap();
        std::thread::sleep(QUIET);
        assert!(load(&dir).unwrap()[0].last_run + 60 >= now());
        assert!(
            probe.started.recv_timeout(QUIET).is_err(),
            "tried a third time"
        );
    }

    #[test]
    fn a_run_cut_off_by_a_quit_is_said_once_at_the_next_start_and_not_run_again() {
        let dir = scratch("cut-off");
        save(&dir, &[every(1, 60, 30)]).unwrap();
        let running = probe(&dir);
        running.runner.run_now(1);
        assert_eq!(running.started.recv_timeout(WAIT).unwrap(), 1);
        // A run going on in a live Letronna is not taken for one cut off.
        let meanwhile = probe(&dir);
        let cut = |probe: &Probe| {
            std::iter::from_fn(|| probe.events.try_recv().ok())
                .filter(|event| matches!(event, Event::Ended(_)))
                .count()
        };
        assert_eq!(cut(&meanwhile), 0);

        // One that died mid-run leaves its marker unlocked.
        let marker = runs_dir(&dir).join("1-0-1.json");
        std::fs::write(&marker, br#"{"schedule": 1, "name": "news"}"#).unwrap();
        let next = probe(&dir);
        let Ok(Event::Ended(ended)) = next.events.try_recv() else {
            panic!("the cut-off run was not said");
        };
        assert_eq!((ended.schedule, ended.name.as_str()), (1, "news"));
        assert!(ended.text.starts_with("Cut off"));
        assert!(!marker.exists());
        assert_eq!(cut(&probe(&dir)), 0, "said twice");
        assert!(next.started.recv_timeout(QUIET).is_err(), "run again");
        running.finish.send(()).unwrap();
    }

    #[test]
    fn a_file_from_before_the_modes_reads_its_switch_as_bypass_or_default_and_saves_modes() {
        use crate::gate::Mode;
        let dir = scratch("old-switch");
        let job = |id: u64, switch: &str| {
            format!(
                r#"{{"id": {id}, "name": "tidy", "prompt": "tidy up", "workspace": null,
                "trigger": {{"Every": {{"minutes": 60}}}}, "enabled": true,
                "last_run": 0{switch}}}"#
            )
        };
        let old = [
            job(1, r#", "unattended": true"#),
            job(2, r#", "unattended": false"#),
            job(3, ""),
        ];
        std::fs::write(path(&dir), format!("[{}]", old.join(", "))).unwrap();
        let modes =
            |dir: &Path| -> Vec<Mode> { load(dir).unwrap().iter().map(|job| job.mode).collect() };
        let read = [Mode::Bypass, Mode::Default, Mode::Default];
        assert_eq!(modes(&dir), read);

        save(&dir, &load(&dir).unwrap()).unwrap();
        let saved: serde_json::Value =
            serde_json::from_slice(&std::fs::read(path(&dir)).unwrap()).unwrap();
        assert_eq!(saved[0]["mode"], "bypass");
        assert!(saved[0].get("unattended").is_none(), "{saved}");
        assert_eq!(modes(&dir), read);
    }

    #[test]
    fn a_mode_from_a_newer_letronna_reads_as_default_and_the_rest_of_the_file_stands() {
        use crate::gate::Mode;
        let dir = scratch("newer-mode");
        let mut saved = serde_json::to_value([every(1, 60, 0), every(2, 60, 0)]).unwrap();
        saved[0]["mode"] = "auto".into();
        saved[1]["mode"] = "sideways".into();
        std::fs::write(path(&dir), serde_json::to_vec(&saved).unwrap()).unwrap();
        let read = load(&dir).unwrap();
        let modes: Vec<Mode> = read.iter().map(|job| job.mode).collect();
        assert_eq!(modes, [Mode::Auto, Mode::Default]);
    }
}
