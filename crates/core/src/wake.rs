//! `wake_me`: end the turn now and have the session woken later.
//!
//! Coming back in twenty minutes used to mean holding the turn open in
//! `wait`, which keeps a round in flight and gives up after two minutes. A
//! wake-up instead ends the turn and, once it comes due, leaves a `Later`
//! notice in the session's mailbox, so nothing runs while the session sleeps
//! and the front end that listens starts its next turn with the notice. A
//! session has one at a time, or a model that arms one every turn would be
//! woken once for each.
//!
//! "Look at CI again tomorrow morning" outlasts most Letronnas, so a pending
//! wake-up waits in `wakes.json` in the data folder, under the short lock
//! the schedules' list takes. Only a front end that starts turns in the
//! session fires it -- the window for every session it lists, the terminal
//! for the one it has open -- since a notice is heard only in the process it
//! is left in. That front end takes the wake-up out of the file under the
//! lock before it leaves the notice, so two Letronnas never both fire it.
//! While it serves any session it looks by the wall clock every minute, as
//! another Letronna may leave one meanwhile and the timer thread's clock
//! stops while the machine sleeps. One missed while nothing served it fires
//! once, saying how late it is; one more than a day late leaves a note for
//! the next turn rather than starting one, and ends a loop waiting on it.
//!
//! `/loop` and `/heartbeat` keep a session going on the same footing. A loop
//! leaves its prompt in the mailbox as a tick, the first at once and each
//! after that an interval on from the end of the last, or, with no interval,
//! when the model's own wake-up comes due. Only the end of the turn that took
//! a tick can say whether it answered LOOP_COMPLETE or asked to be woken, so
//! the front ends report every turn's end and the loop picks out its own. A
//! session has one loop, held in memory only, so a restart ends it.

use crate::chat::{self as backend, Connection};
use crate::inbox::{self, Notice, When};
use crate::schedule;
use crate::timers::{self, TimerId};
use crate::tools::{Tool, ToolContext, one_line};
use chrono::{Local, TimeZone};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

/// A minute: anything sooner is a pause within the turn, which `wait` keeps
/// without ending it.
const SOONEST: f64 = 60.;

/// A day, as far as "tomorrow morning": anything further off, or anything
/// that repeats, is a schedule's, which runs on a prompt of its own.
const LATEST: f64 = 86_400.;

/// What a woken session is told to do when the model named nothing else.
const CARRY_ON: &str = "Carry on where you left off.";

/// How often a front end looks at the wake-ups of the sessions it serves. A
/// minute is also the most a sleeping machine can make one late once it is
/// awake again, and how late one has to be to have been missed.
const BEAT: Duration = Duration::from_secs(60);

/// A day: a wake-up missed by longer, as over a weekend away, was about a
/// moment long gone, so it leaves a note instead of starting a turn.
const STALE: u64 = 24 * 60 * 60;

/// Where a wake-up comes from in the mailbox.
const WAKE_UP: &str = "wake-up";

/// A wake-up waiting in `wakes.json`, where it outlives the Letronna that
/// armed it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct Wake {
    session: u64,
    /// When it comes due, in epoch seconds by the wall clock, which goes on
    /// while the machine sleeps.
    due: u64,
    reason: String,
    prompt: String,
    /// When it was armed, in epoch seconds.
    armed: u64,
}

/// The sessions whose wake-ups this Letronna fires, and its next look.
struct Served {
    sessions: BTreeSet<u64>,
    /// Set while it serves any.
    timer: Option<TimerId>,
}

/// What this Letronna serves, none until wake_me or `serve` names one.
static SERVED: Mutex<Served> = Mutex::new(Served {
    sessions: BTreeSet::new(),
    timer: None,
});

/// Where a loop's ticks come from in the mailbox, which is also how the turn
/// that took one is told from a turn that ran while it waited.
const TICK: &str = "loop";

/// A minute: a loop coming round sooner would be one long turn, which `wait`
/// or a job's notice does without a model call each time.
const SOONEST_TICK: Duration = Duration::from_secs(60);

/// A day: rarer than that is a schedule's work, which outlives a restart
/// where a loop does not.
const LATEST_TICK: Duration = Duration::from_secs(24 * 60 * 60);

/// Twenty minutes, the least wake_me's description asks of a fallback, so
/// whatever the tick was waiting for has time to send its notice first.
const FALLBACK: Duration = Duration::from_secs(20 * 60);

/// A working day of ticks five minutes apart, so a loop nobody watches
/// cannot spend without end.
const MAX_TICKS: u32 = 100;

/// A week, so a loop forgotten in a session ends before it has cost much.
const MAX_AGE: Duration = Duration::from_secs(7 * 24 * 60 * 60);

/// What the model answers, on a line of its own, when the loop's work is done.
const COMPLETE: &str = "LOOP_COMPLETE";

/// What a heartbeat looks at when it is given nothing else.
const HEARTBEAT: &str =
    "Look over what this session is doing and whether anything needs attention.";

/// Each session's loop.
static LOOPS: Mutex<BTreeMap<u64, Loop>> = Mutex::new(BTreeMap::new());

/// Numbers every loop, so a timer set by one that was replaced does nothing.
static LOOP_IDS: AtomicU64 = AtomicU64::new(0);

pub struct WakeMe;
impl Tool for WakeMe {
    fn name(&self) -> &'static str {
        "wake_me"
    }
    fn description(&self) -> &'static str {
        "End your turn and have this session woken later, such as to look at CI or a deploy again in 20 minutes. Never wake early to poll for something that sends a notice when it is done. Keep a fallback wake-up, for a notice that may not come, at 20 minutes or more. A session has one wake-up: a newer one replaces it."
    }
    fn parameters(&self) -> Value {
        json!({"type": "object", "properties": {
            "seconds": {"type": "integer", "description": "60 to 86400."},
            "reason": {"type": "string", "description": "Shown to the user."},
            "prompt": {"type": "string", "description": "What to do on waking; defaults to carrying on."}
        }, "required": ["seconds", "reason"]})
    }
    fn offered(&self, connection: &Connection) -> bool {
        woken(inbox::listened(), connection)
    }
    fn run(&self, args: &Value, ctx: &ToolContext) -> Result<String, String> {
        let seconds = args["seconds"]
            .as_f64()
            .ok_or("`seconds` is required: how long to sleep, 60 to 86400.")?;
        let reason = one_line(args["reason"].as_str().unwrap_or_default());
        if reason.is_empty() {
            return Err(
                "`reason` is required: a few words the user sees, such as \"check CI\".".into(),
            );
        }
        let prompt = match args["prompt"].as_str().map(str::trim) {
            Some(prompt) if !prompt.is_empty() => prompt,
            _ => CARRY_ON,
        };
        let delay = delay(seconds);
        let session = ctx.conversation;
        let now = backend::now();
        let dir = backend::data_dir();
        put(
            &dir,
            Wake {
                session,
                due: now + delay.as_secs(),
                reason: reason.clone(),
                prompt: prompt.into(),
                armed: now,
            },
        )?;
        // Its turn runs here, so this front end serves it.
        served().sessions.insert(session);
        look_in(dir, delay.min(BEAT));
        asked_to_wake(session, delay);
        let at = Local::now() + delay;
        let tomorrow = if at.date_naive() == Local::now().date_naive() {
            ""
        } else {
            " tomorrow"
        };
        let minutes = (delay.as_secs() + 30) / 60;
        // Past two hours, minutes are harder to read than the hours they make.
        let (count, unit) = if minutes < 120 {
            (minutes, "minute")
        } else {
            ((minutes + 30) / 60, "hour")
        };
        let s = if count == 1 { "" } else { "s" };
        Ok(format!(
            "This session wakes at {}{tomorrow}, in {count} {unit}{s} (\"{reason}\"). End your turn now. A newer wake_me replaces this one.",
            at.format("%H:%M")
        ))
    }
}

/// Whether a session is woken once its turn ends, which takes a listening
/// front end and no chat app, whose answer would have nowhere to go.
fn woken(listened: bool, connection: &Connection) -> bool {
    listened && connection.remote.is_none()
}

/// The sleep asked for, held to between a minute and a day.
fn delay(seconds: f64) -> Duration {
    Duration::from_secs_f64(seconds.clamp(SOONEST, LATEST))
}

/// Fire the wake-ups of `sessions` and no other session's from now on, any
/// missed while nothing served them at once.
pub fn serve(sessions: impl IntoIterator<Item = u64>) {
    // A Letronna that starts no turn between prompts would take a wake-up
    // out of the file into a mailbox nobody reads.
    if !inbox::listened() {
        return;
    }
    served().sessions = sessions.into_iter().collect();
    look_in(backend::data_dir(), Duration::ZERO);
}

/// Take back the session's pending wake-up, if it has one.
pub fn cancel(session: u64) -> Result<(), String> {
    let dir = backend::data_dir();
    // With no file there is nothing to take back, and no lock to make.
    if !path(&dir).exists() {
        return Ok(());
    }
    edit(&dir, |wakes| wakes.retain(|wake| wake.session != session))
}

/// Where the wake-ups wait in data folder `dir`.
fn path(dir: &Path) -> PathBuf {
    dir.join("wakes.json")
}

/// The wake-ups on disk: none when there is no file, and why not when there
/// is one this build cannot read, which is then never saved over.
fn load(dir: &Path) -> Result<Vec<Wake>, String> {
    let bytes = match std::fs::read(path(dir)) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(format!("Could not read the wake-ups: {e}")),
    };
    serde_json::from_slice(&bytes).map_err(|e| {
        format!(
            "The wake-ups in {} cannot be read ({e}), so nothing is saved over them until the file is fixed.",
            path(dir).display()
        )
    })
}

/// Write the wake-ups whole through a file of this process's own, so a
/// Letronna reading meanwhile finds the old list or the new one.
fn save(dir: &Path, wakes: &[Wake]) -> Result<(), String> {
    let failed = |e: std::io::Error| format!("Could not save the wake-ups: {e}");
    // With none left the file goes, so a look finds nothing without taking
    // the lock.
    if wakes.is_empty() {
        return match std::fs::remove_file(path(dir)) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(failed(e)),
            _ => Ok(()),
        };
    }
    let bytes = serde_json::to_vec_pretty(wakes)
        .map_err(|e| format!("Could not save the wake-ups: {e}"))?;
    let temp = path(dir).with_extension(format!("{}.tmp", std::process::id()));
    std::fs::write(&temp, bytes)
        .and_then(|()| std::fs::rename(&temp, path(dir)))
        .map_err(|e| {
            let _ = std::fs::remove_file(&temp);
            failed(e)
        })
}

/// Read the wake-ups, let `change` change them and save what it leaves, all
/// under the schedules' short lock, so no other Letronna's change lands in
/// between and is lost.
fn edit<T>(dir: &Path, change: impl FnOnce(&mut Vec<Wake>) -> T) -> Result<T, String> {
    let _held = lock(dir)?;
    let mut wakes = load(dir)?;
    let out = change(&mut wakes);
    save(dir, &wakes)?;
    Ok(out)
}

/// Hold the schedules' short lock, which the wake-ups' file is kept under
/// too, until the file goes.
fn lock(dir: &Path) -> Result<std::fs::File, String> {
    schedule::hold_list_lock(dir).map_err(|e| format!("Could not lock the wake-ups: {e}"))
}

/// Keep `wake` in place of any its session had.
fn put(dir: &Path, wake: Wake) -> Result<(), String> {
    edit(dir, |wakes| {
        wakes.retain(|other| other.session != wake.session);
        wakes.push(wake);
    })
}

/// Take the wake-ups of `served` sessions due by `now` out of the file under
/// its lock, so no other Letronna fires them too, with when the next of
/// theirs still waiting comes due.
fn claim(dir: &Path, served: &BTreeSet<u64>, now: u64) -> Result<(Vec<Wake>, Option<u64>), String> {
    if !path(dir).exists() {
        return Ok((Vec::new(), None));
    }
    let _held = lock(dir)?;
    let (due, waiting): (Vec<Wake>, Vec<Wake>) = load(dir)?
        .into_iter()
        .partition(|wake| served.contains(&wake.session) && wake.due <= now);
    if !due.is_empty() {
        save(dir, &waiting)?;
    }
    let next = waiting
        .iter()
        .filter(|wake| served.contains(&wake.session))
        .map(|wake| wake.due)
        .min();
    Ok((due, next))
}

/// Leave a claimed wake-up in its session's mailbox, saying when it was due
/// if it was missed, or only a line for the next turn once a day late.
fn deliver(wake: &Wake, now: u64) {
    let late = now.saturating_sub(wake.due);
    let due = clock(wake.due, now);
    if late > STALE {
        // A loop the model paces waits for this as its next tick, and with
        // no tick coming it would wait for good.
        let mut loops = loops();
        let paced = (loops.get(&wake.session)).is_some_and(|looping| looping.pace == Pace::Model);
        let ended = if paced {
            loops.remove(&wake.session)
        } else {
            None
        };
        drop(loops);
        let stopped = match ended {
            Some(looping) => {
                looping.end(wake.session);
                " and stopped the /loop waiting on it"
            }
            None => "",
        };
        inbox::push(
            wake.session,
            Notice {
                from: WAKE_UP.into(),
                when: When::Quiet,
                text: format!(
                    "wake-up \"{}\", due {due}, came more than a day late, so it started no turn{stopped}.",
                    wake.reason
                ),
            },
        );
        return;
    }
    let mut text = format!("wake-up: {}", wake.reason);
    if late > BEAT.as_secs() {
        text.push_str(&format!(" (due {due}, {})", late_by(late)));
    }
    text.push('\n');
    text.push_str(&wake.prompt);
    if wake_loop(wake.session, &text) {
        return;
    }
    inbox::push(
        wake.session,
        Notice {
            from: WAKE_UP.into(),
            when: When::Later,
            text,
        },
    );
}

/// An epoch second as the time of day, with the weekday when it was not
/// today as of `now`.
fn clock(at: u64, now: u64) -> String {
    let local = |second: u64| Local.timestamp_opt(second as i64, 0).single();
    match (local(at), local(now)) {
        (Some(at), Some(now)) if at.date_naive() == now.date_naive() => {
            at.format("%H:%M").to_string()
        }
        (Some(at), _) => at.format("%a %H:%M").to_string(),
        (None, _) => "earlier".into(),
    }
}

/// How late a wake-up is, in words: "42 min late", or "5 h late" once
/// minutes are more than can be read at a glance.
fn late_by(seconds: u64) -> String {
    let minutes = (seconds + 30) / 60;
    if minutes < 120 {
        format!("{minutes} min late")
    } else {
        format!("{} h late", (minutes + 30) / 60)
    }
}

/// Fire what is due among the wake-ups this Letronna serves in data folder
/// `dir`, and look again while it serves any.
fn look(dir: PathBuf) {
    let served = served().sessions.clone();
    if served.is_empty() {
        return;
    }
    let now = backend::now();
    let wait = match claim(&dir, &served, now) {
        Ok((due, next)) => {
            for wake in &due {
                deliver(wake, now);
            }
            // With none waiting it still looks a beat on, as another
            // Letronna may leave one for a session served here.
            next.map_or(BEAT, |at| {
                Duration::from_secs(at.saturating_sub(now)).min(BEAT)
            })
        }
        // A lock or a save that failed may work a beat on.
        Err(_) => BEAT,
    };
    look_in(dir, wait);
}

/// Look after `wait`, in place of the look set before.
fn look_in(dir: PathBuf, wait: Duration) {
    // Held until the timer is listed, or a look it starts at once could list
    // its next one first and have it taken back here.
    let mut served = served();
    // A look reads and writes a file, which a timer's callback is too quick
    // for, so it gets a thread of its own, or runs right there when there is
    // none to be had.
    let timer = timers::at(
        Instant::now() + wait,
        Box::new(move || {
            let there = dir.clone();
            if std::thread::Builder::new()
                .name("wake-ups".into())
                .spawn(move || look(there))
                .is_err()
            {
                look(dir);
            }
        }),
    );
    if let Some(older) = served.timer.replace(timer) {
        timers::cancel(older);
    }
}

/// What this Letronna serves, which is whole even after a panic because
/// nothing is left half done under the lock.
fn served() -> MutexGuard<'static, Served> {
    SERVED.lock().unwrap_or_else(PoisonError::into_inner)
}

/// How a loop comes round again.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Pace {
    /// This long after its last tick ended.
    Every(Duration),
    /// The same, for a heartbeat, which is told not to invent work.
    Heartbeat(Duration),
    /// When the model's wake_me comes due, with one fallback when it asks
    /// for none.
    Model,
}

impl Pace {
    /// The command that starts a loop at this pace.
    fn command(self) -> &'static str {
        match self {
            Pace::Heartbeat(_) => "/heartbeat",
            _ => "/loop",
        }
    }

    /// How often, in words, such as "every 10 minutes".
    fn words(self) -> String {
        match self {
            Pace::Every(every) | Pace::Heartbeat(every) => format!("every {}", span(every)),
            Pace::Model => "at the model's own pace".into(),
        }
    }

    /// What each tick asks of the model after the prompt.
    fn preamble(self) -> String {
        match self {
            Pace::Every(every) => format!(
                "If the work is finished for good, answer {COMPLETE} on a line of its own; otherwise this comes round again in {}.",
                span(every)
            ),
            Pace::Heartbeat(_) => {
                "If nothing changed, reply briefly and stop; do not invent work.".into()
            }
            Pace::Model => format!(
                "When this tick is done, call wake_me to come back when it is worth looking again, or answer {COMPLETE} on a line of its own if the work is finished."
            ),
        }
    }
}

/// A session's loop, from `/loop` or `/heartbeat`.
struct Loop {
    /// Tells this loop's timers from those of one it replaced.
    id: u64,
    pace: Pace,
    prompt: String,
    /// Ticks sent so far.
    ticks: u32,
    started: Instant,
    /// Set from sending a tick until the turn that took it ends.
    due: bool,
    /// Whether the model called wake_me since the last tick went out.
    woke: bool,
    /// Whether the last tick asked for no wake-up and none was asked for
    /// since, so another such tick ends a loop the model paces.
    missed: bool,
    /// When the next tick goes, if a timer is set for it.
    timer: Option<TimerId>,
}

impl Loop {
    /// Leave the next tick in the session's mailbox, with the wake-up that
    /// brought it, if one did.
    fn tick(&mut self, session: u64, woken: Option<&str>) {
        self.ticks += 1;
        self.due = true;
        self.woke = false;
        let pace = self.pace;
        // A notice opens by saying it is not from the user, and only its
        // first line, which Letronna writes, stands at the margin: that
        // line is where to say the prompt under it is the user's own.
        let mut text = format!(
            "{} tick {}, {}. The user set this loop up, and this is their prompt:\n{}",
            pace.command(),
            self.ticks,
            pace.words(),
            self.prompt
        );
        if let Some(woken) = woken {
            text.push('\n');
            text.push_str(woken);
        }
        text.push('\n');
        text.push_str(&pace.preamble());
        inbox::push(
            session,
            Notice {
                from: TICK.into(),
                when: When::Later,
                text,
            },
        );
    }

    /// Set the next tick going after one ended, or say why there is none.
    fn again(&mut self, session: u64) -> Option<String> {
        let wait = match self.pace {
            Pace::Every(every) | Pace::Heartbeat(every) => every,
            // Its wake-up is the next tick.
            Pace::Model if self.woke => return None,
            Pace::Model if self.missed => {
                return Some(format!(
                    "The loop stopped: two ticks in a row neither called wake_me nor answered {COMPLETE}."
                ));
            }
            Pace::Model => {
                self.missed = true;
                FALLBACK
            }
        };
        let id = self.id;
        self.timer = Some(timers::at(
            Instant::now() + wait,
            Box::new(move || fire(session, id)),
        ));
        None
    }

    /// The loop in words, such as "/loop every 10 minutes, 3 ticks: check CI".
    fn about(&self) -> String {
        let s = if self.ticks == 1 { "" } else { "s" };
        format!(
            "{} {}, {} tick{s}: {}",
            self.pace.command(),
            self.pace.words(),
            self.ticks,
            one_line(&self.prompt)
        )
    }

    /// Take back what the loop still has coming: its timer, a tick nobody
    /// took, and the wake-up that would be the next tick of one the model
    /// paces.
    fn end(self, session: u64) {
        if let Some(timer) = self.timer {
            timers::cancel(timer);
        }
        inbox::withdraw(session, TICK);
        // One a failed take-back leaves fires once as a plain wake-up.
        if self.pace == Pace::Model {
            let _ = cancel(session);
        }
    }
}

/// Carry out `/loop` or `/heartbeat` in a session and say how it went in a
/// line; `None` when `name` is neither.
pub fn command(session: u64, name: &str, rest: &str) -> Option<Result<String, String>> {
    let heartbeat = match name {
        "/loop" => false,
        "/heartbeat" => true,
        _ => return None,
    };
    Some(parse(heartbeat, rest).and_then(|asked| match asked {
        Asked::Show => {
            Ok(looping(session).unwrap_or_else(|| {
                "No loop in this session. /loop 10m <prompt> starts one.".into()
            }))
        }
        Asked::Stop => Ok(stop(session).unwrap_or_else(|| "No loop to stop.".into())),
        Asked::Start(pace, prompt) => start(session, pace, prompt),
    }))
}

/// What the session's loop does, in a line, or `None` when it has none.
pub fn looping(session: u64) -> Option<String> {
    loops().get(&session).map(Loop::about)
}

/// End the session's loop and say which it was, or `None` when it had none.
pub fn stop(session: u64) -> Option<String> {
    let looping = loops().remove(&session)?;
    let about = looping.about();
    looping.end(session);
    Some(format!("Stopped {about}"))
}

/// Whether the session's loop has a tick waiting for a turn to take it.
pub fn waiting(session: u64) -> bool {
    inbox::holds(session, TICK)
}

/// Move the session's loop on once a turn there ended, given the turn's last
/// answer, or `None` when the user stopped it; says so when that ended the
/// loop.
pub fn turn_ended(session: u64, answer: Option<&str>) -> Option<String> {
    let mut loops = loops();
    let looping = loops.get_mut(&session)?;
    // A turn that ended while the tick still waited was not the tick's.
    if !looping.due || inbox::holds(session, TICK) {
        return None;
    }
    looping.due = false;
    let ended = match answer {
        None => Some("Stopping its tick stopped the loop too.".to_owned()),
        Some(answer) if complete(answer) => {
            Some(format!("The loop is done: its tick answered {COMPLETE}."))
        }
        Some(_) if looping.ticks >= MAX_TICKS => Some(format!(
            "The loop stopped after {MAX_TICKS} ticks, the most one runs."
        )),
        Some(_) if looping.started.elapsed() >= MAX_AGE => {
            Some("The loop stopped after a week, the longest one runs.".into())
        }
        Some(_) => looping.again(session),
    };
    if ended.is_some()
        && let Some(looping) = loops.remove(&session)
    {
        drop(loops);
        looping.end(session);
    }
    ended
}

/// What `/loop` or `/heartbeat` asked for.
#[derive(Debug, PartialEq)]
enum Asked {
    /// Start a loop, in place of any the session had.
    Start(Pace, String),
    Stop,
    Show,
}

/// Read what follows `/loop` or `/heartbeat`: nothing to show the loop,
/// `stop` to end it, or an interval and a prompt, either of which a
/// heartbeat or a loop may leave out.
fn parse(heartbeat: bool, rest: &str) -> Result<Asked, String> {
    let rest = rest.trim();
    if rest.is_empty() {
        return Ok(Asked::Show);
    }
    if rest.eq_ignore_ascii_case("stop") {
        return Ok(Asked::Stop);
    }
    let (first, after) = rest.split_once(char::is_whitespace).unwrap_or((rest, ""));
    let every = interval(first)?;
    let prompt = if every.is_some() { after.trim() } else { rest };
    match every {
        Some(every) if heartbeat => {
            let prompt = if prompt.is_empty() { HEARTBEAT } else { prompt };
            Ok(Asked::Start(Pace::Heartbeat(every), prompt.into()))
        }
        None if heartbeat => {
            Err("A heartbeat needs how often, in minutes: /heartbeat 30 check the deploy.".into())
        }
        _ if prompt.is_empty() => Err("A loop needs something to do: /loop 10m check CI.".into()),
        Some(every) => Ok(Asked::Start(Pace::Every(every), prompt.into())),
        None => Ok(Asked::Start(Pace::Model, prompt.into())),
    }
}

/// Read an interval such as `90s`, `10m`, `2h` or `1d`, a bare number being
/// minutes; `None` when the word is not one.
fn interval(word: &str) -> Result<Option<Duration>, String> {
    let digits = word.len() - word.trim_start_matches(|c: char| c.is_ascii_digit()).len();
    let (number, unit) = word.split_at(digits);
    let seconds: u64 = match unit {
        "s" => 1,
        "" | "m" | "min" => 60,
        "h" => 60 * 60,
        "d" => 24 * 60 * 60,
        _ => return Ok(None),
    };
    if number.is_empty() {
        return Ok(None);
    }
    // Digits too many to count are far past the longest interval anyway.
    let number = number.parse::<u64>().unwrap_or(u64::MAX);
    let every = Duration::from_secs(number.saturating_mul(seconds));
    if !(SOONEST_TICK..=LATEST_TICK).contains(&every) {
        return Err(
            "A loop comes round from once a minute to once a day, such as 10m or 2h.".into(),
        );
    }
    Ok(Some(every))
}

/// Start a loop in a session, in place of any it had, with its first tick
/// sent now so it is seen working before it waits.
fn start(session: u64, pace: Pace, prompt: String) -> Result<String, String> {
    // Ticks wait in the mailbox for a front end to start their turns, which
    // a one-shot run or a pipe never does.
    if !inbox::listened() {
        return Err(
            "Nothing here starts a turn between prompts, so a loop would never come round.".into(),
        );
    }
    let replaced = stop(session);
    let mut looping = Loop {
        id: LOOP_IDS.fetch_add(1, Ordering::Relaxed),
        pace,
        prompt,
        ticks: 0,
        started: Instant::now(),
        due: false,
        woke: false,
        missed: false,
        timer: None,
    };
    let mut loops = loops();
    looping.tick(session, None);
    loops.insert(session, looping);
    let what = match pace {
        Pace::Heartbeat(_) => "Heartbeat",
        _ => "Looping",
    };
    let instead = replaced.map_or(String::new(), |_| " in place of the last loop".into());
    Ok(format!(
        "{what} {}, from now{instead}. /loop stop ends it.",
        pace.words()
    ))
}

/// A loop's timer came due: send its next tick, unless the loop was replaced.
fn fire(session: u64, id: u64) {
    let mut loops = loops();
    let Some(looping) = loops.get_mut(&session).filter(|looping| looping.id == id) else {
        return;
    };
    looping.timer = None;
    looping.tick(session, None);
}

/// Send a wake-up as the next tick of a loop the model paces; false when the
/// session has no such loop, and the wake-up goes as it is.
fn wake_loop(session: u64, woken: &str) -> bool {
    let mut loops = loops();
    let Some(looping) = loops
        .get_mut(&session)
        .filter(|looping| looping.pace == Pace::Model)
    else {
        return false;
    };
    // A fallback armed for the last tick would send one more after this.
    if let Some(timer) = looping.timer.take() {
        timers::cancel(timer);
    }
    looping.tick(session, Some(woken));
    true
}

/// Note that the model asked to be woken in `delay`, which in a loop it paces
/// puts the fallback off until after the wake-up and forgives a tick that
/// asked for nothing.
fn asked_to_wake(session: u64, delay: Duration) {
    let mut loops = loops();
    let Some(looping) = loops.get_mut(&session) else {
        return;
    };
    looping.woke = true;
    looping.missed = false;
    if looping.pace == Pace::Model {
        // The wake-up is the next tick, unless another Letronna serving the
        // session fires it, which would leave this loop waiting for good.
        let id = looping.id;
        let fallback = timers::at(
            Instant::now() + delay + FALLBACK,
            Box::new(move || fire(session, id)),
        );
        if let Some(older) = looping.timer.replace(fallback) {
            timers::cancel(older);
        }
    }
}

/// Whether an answer says the loop is done on a line of its own; the word
/// inside a sentence, as in "I will say LOOP_COMPLETE once it passes", does
/// not end it.
fn complete(answer: &str) -> bool {
    answer.lines().any(|line| {
        let line = line
            .trim()
            .trim_matches(|c| matches!(c, '*' | '`' | '_' | '.'));
        line == COMPLETE || line == "LOOP COMPLETE"
    })
}

/// A duration in words, in the largest unit that divides it: "10 minutes".
fn span(duration: Duration) -> String {
    let seconds = duration.as_secs();
    let (count, unit) = [(24 * 60 * 60, "day"), (60 * 60, "hour"), (60, "minute")]
        .into_iter()
        .find(|(size, _)| seconds.is_multiple_of(*size))
        .map_or((seconds, "second"), |(size, unit)| (seconds / size, unit));
    let s = if count == 1 { "" } else { "s" };
    format!("{count} {unit}{s}")
}

/// The loops, which are whole even after a panic because nothing is left
/// half done under the lock.
fn loops() -> MutexGuard<'static, BTreeMap<u64, Loop>> {
    LOOPS.lock().unwrap_or_else(PoisonError::into_inner)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chat::Remote;
    use crate::store::tests::{DATA_DIR, Scratch};
    use crate::tools::{run, specs};
    use std::sync::{Arc, Barrier};

    /// A wake-up for `session` due at epoch second `due`.
    fn wake(session: u64, due: u64, reason: &str) -> Wake {
        Wake {
            session,
            due,
            reason: reason.into(),
            prompt: CARRY_ON.into(),
            armed: due.saturating_sub(3_600),
        }
    }

    /// A data folder of the test's own, set as `AGENT_DATA_DIR` while the
    /// guards are held, for code that finds the wake-ups through it.
    fn data_folder(name: &str) -> (MutexGuard<'static, ()>, MutexGuard<'static, ()>, Scratch) {
        let data = DATA_DIR.lock().unwrap_or_else(|e| e.into_inner());
        let bell = inbox::tests::DOORBELL
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let dir = Scratch::new(name);
        unsafe { std::env::set_var("AGENT_DATA_DIR", &*dir) };
        (data, bell, dir)
    }

    #[test]
    fn a_newer_wake_replaces_the_pending_one() {
        let dir = Scratch::new("wakes-newer");
        put(&dir, wake(901, 100, "older")).unwrap();
        put(&dir, wake(902, 100, "other")).unwrap();
        put(&dir, wake(901, 200, "newer")).unwrap();
        // Another session's wake-up is its own, and still pending.
        let reasons: Vec<String> = load(&dir).unwrap().into_iter().map(|w| w.reason).collect();
        assert_eq!(reasons, ["other", "newer"]);
    }

    #[test]
    fn delays_are_clamped_to_a_minute_and_a_day() {
        for (asked, slept) in [
            (-5., 60),
            (0., 60),
            (5., 60),
            (90., 90),
            (1_200., 1_200),
            (86_400., 86_400),
            (200_000., 86_400),
        ] {
            assert_eq!(delay(asked), Duration::from_secs(slept), "{asked}");
        }
    }

    #[test]
    fn one_shot_runs_are_refused() {
        // `-p` never listens for notices: its session ends with the run.
        let session = Connection {
            conversation: 903,
            ..Default::default()
        };
        assert!(!woken(false, &session));
        assert!(woken(true, &session));

        // Nor is a chat app's session woken, and naming the tool there
        // anyway is refused rather than armed.
        let chat = Connection {
            conversation: 904,
            remote: Some(Remote::default()),
            ..Default::default()
        };
        assert!(!woken(true, &chat));
        assert!(
            !specs(&chat)
                .iter()
                .any(|t| t["function"]["name"] == "wake_me")
        );
        let refused = run(
            "wake_me",
            r#"{"seconds": 1200, "reason": "check CI"}"#,
            &ToolContext::of(&chat),
        )
        .unwrap_err();
        assert!(refused.contains("not available"), "{refused}");
        assert!(!served().sessions.contains(&904));
    }

    /// Stand in for the timer of a session's loop coming due, taking back
    /// the real one so it cannot fire later in another test's way.
    fn come_due(session: u64) {
        let (id, timer) = {
            let loops = loops();
            let looping = &loops[&session];
            (
                looping.id,
                looping.timer.expect("a timer for the next tick"),
            )
        };
        timers::cancel(timer);
        fire(session, id);
    }

    /// The tick waiting for a session, taken as a front end takes it.
    fn take_tick(session: u64) -> String {
        let taken = inbox::take(session, true);
        assert_eq!(taken.len(), 1, "{taken:?}");
        taken[0].text.clone()
    }

    #[test]
    fn loop_intervals_parse() {
        let start = |pace, prompt: &str| Ok(Asked::Start(pace, prompt.to_owned()));
        let minutes = |n: u64| Duration::from_secs(n * 60);
        assert_eq!(
            parse(false, "10m check CI"),
            start(Pace::Every(minutes(10)), "check CI")
        );
        assert_eq!(
            parse(false, "  2h   tidy the branch "),
            start(Pace::Every(minutes(120)), "tidy the branch")
        );
        assert_eq!(
            parse(false, "90s look"),
            start(Pace::Every(Duration::from_secs(90)), "look")
        );
        assert_eq!(
            parse(false, "1d look"),
            start(Pace::Every(minutes(24 * 60)), "look")
        );
        assert_eq!(
            parse(false, "5 look"),
            start(Pace::Every(minutes(5)), "look")
        );
        // With no interval the model sets the pace, and a word that only
        // starts with a number is part of the prompt.
        assert_eq!(
            parse(false, "check CI until it is green"),
            start(Pace::Model, "check CI until it is green")
        );
        assert_eq!(
            parse(false, "10x the tests"),
            start(Pace::Model, "10x the tests")
        );
        assert_eq!(parse(false, " "), Ok(Asked::Show));
        assert_eq!(parse(false, " stop "), Ok(Asked::Stop));
        assert_eq!(parse(true, "stop"), Ok(Asked::Stop));
        assert_eq!(parse(false, "Stop"), Ok(Asked::Stop));
        for refused in [
            "30s look",
            "2d look",
            "0 look",
            "10m",
            "99999999999999999999h x",
        ] {
            assert!(parse(false, refused).is_err(), "{refused}");
        }
        assert_eq!(
            parse(true, "30 check the deploy"),
            start(Pace::Heartbeat(minutes(30)), "check the deploy")
        );
        assert_eq!(
            parse(true, "45m"),
            start(Pace::Heartbeat(minutes(45)), HEARTBEAT)
        );
        assert!(parse(true, "check the deploy").is_err());
        assert_eq!(span(minutes(10)), "10 minutes");
        assert_eq!(span(Duration::from_secs(90)), "90 seconds");
        assert_eq!(span(minutes(60)), "1 hour");
    }

    #[test]
    fn loop_complete_on_its_own_line_ends_the_loop() {
        for done in [
            "LOOP_COMPLETE",
            "CI is green.\nLOOP_COMPLETE",
            "All done.\r\n\n**LOOP_COMPLETE**\n",
            "`LOOP_COMPLETE`",
            "LOOP COMPLETE",
        ] {
            assert!(complete(done), "{done:?}");
        }
        for going in [
            "I will answer LOOP_COMPLETE once CI is green.",
            "Still red.",
            "Loop complete",
            "LOOP_COMPLETED",
        ] {
            assert!(!complete(going), "{going:?}");
        }

        let _bell = inbox::tests::DOORBELL
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        inbox::listen();
        let session = 911;
        let said = command(session, "/loop", "10m check CI").unwrap().unwrap();
        assert!(said.contains("every 10 minutes"), "{said}");
        // A turn that ended while the tick still waited was not the tick's.
        assert_eq!(turn_ended(session, Some("LOOP_COMPLETE")), None);
        assert!(looping(session).is_some());
        let tick = take_tick(session);
        assert!(
            tick.starts_with(
                "/loop tick 1, every 10 minutes. The user set this loop up, and this is their prompt:\ncheck CI\n"
            ),
            "{tick}"
        );
        assert!(
            tick.contains("LOOP_COMPLETE on a line of its own"),
            "{tick}"
        );
        // The word inside a sentence goes round again.
        assert_eq!(
            turn_ended(
                session,
                Some("Still red. I will say LOOP_COMPLETE when it passes.")
            ),
            None
        );
        come_due(session);
        assert!(take_tick(session).starts_with("/loop tick 2,"));
        let ended = turn_ended(session, Some("CI is green.\nLOOP_COMPLETE")).unwrap();
        assert!(ended.contains("done"), "{ended}");
        assert!(looping(session).is_none());
        assert!(inbox::take(session, true).is_empty());
    }

    #[test]
    fn a_tick_without_wake_me_gets_one_fallback() {
        let (_data, _bell, dir) = data_folder("wakes-fallback");
        inbox::listen();
        let session = 912;
        command(session, "/loop", "watch the deploy")
            .unwrap()
            .unwrap();
        assert!(take_tick(session).contains("call wake_me"));

        // A tick that asked to be woken keeps a fallback only for a wake-up
        // another Letronna fires: its own wake-up is the next tick.
        let ctx = ToolContext {
            conversation: session,
            ..Default::default()
        };
        run(
            "wake_me",
            r#"{"seconds": 1200, "reason": "rollout at 40%"}"#,
            &ctx,
        )
        .unwrap();
        assert_eq!(turn_ended(session, Some("Back in 20 minutes.")), None);
        assert!(loops()[&session].timer.is_some());
        let due = load(&dir).unwrap()[0].due;
        let (woken, _) = claim(&dir, &BTreeSet::from([session]), due).unwrap();
        deliver(&woken[0], due);
        assert!(loops()[&session].timer.is_none());
        let tick = take_tick(session);
        assert!(tick.starts_with("/loop tick 2,"), "{tick}");
        assert!(tick.contains("\nwake-up: rollout at 40%\n"), "{tick}");

        // One that asked for nothing gets one fallback, and a second such
        // tick in a row ends the loop.
        assert_eq!(turn_ended(session, Some("Still rolling out.")), None);
        come_due(session);
        assert!(take_tick(session).starts_with("/loop tick 3,"));
        let ended = turn_ended(session, Some("Still rolling out.")).unwrap();
        assert!(ended.contains("wake_me"), "{ended}");
        assert!(looping(session).is_none());

        // Stopping a loop the model paces takes back the wake-up that would
        // have been its next tick.
        command(session, "/loop", "watch the deploy")
            .unwrap()
            .unwrap();
        take_tick(session);
        run("wake_me", r#"{"seconds": 1200, "reason": "rollout"}"#, &ctx).unwrap();
        assert_eq!(load(&dir).unwrap().len(), 1);
        command(session, "/loop", "stop").unwrap().unwrap();
        assert!(load(&dir).unwrap().is_empty());
        unsafe { std::env::remove_var("AGENT_DATA_DIR") };
    }

    #[test]
    fn stopping_a_loop_takes_back_its_waiting_tick() {
        let _bell = inbox::tests::DOORBELL
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        inbox::listen();
        let session = 913;
        command(session, "/heartbeat", "30").unwrap().unwrap();
        assert!(waiting(session));
        let about = looping(session).unwrap();
        assert!(
            about.starts_with("/heartbeat every 30 minutes, 1 tick:"),
            "{about}"
        );
        let said = command(session, "/loop", "stop").unwrap().unwrap();
        assert!(said.contains("/heartbeat every 30 minutes"), "{said}");
        assert!(!waiting(session));
        assert!(inbox::take(session, true).is_empty());
        assert!(command(session, "/where", "").is_none());

        // Stopping the turn that took a tick stops its loop too.
        command(session, "/loop", "5m look").unwrap().unwrap();
        take_tick(session);
        assert!(turn_ended(session, None).is_some());
        assert!(looping(session).is_none());
    }

    #[test]
    fn a_wake_armed_before_a_restart_fires_once_after_it() {
        let _bell = inbox::tests::DOORBELL
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let dir = Scratch::new("wakes-restart");
        let session = 921;
        // All that is left of it once the Letronna that armed it quit.
        let mut armed = wake(session, 1_000_000, "check CI");
        armed.prompt = "Look at the last run.".into();
        put(&dir, armed).unwrap();
        let served = BTreeSet::from([session]);
        // A Letronna that does not serve the session leaves it be.
        let missed = 1_000_000 + 42 * 60;
        let other = BTreeSet::from([922]);
        assert_eq!(claim(&dir, &other, missed).unwrap(), (Vec::new(), None));
        // One that does names when it is due, until it is.
        assert_eq!(
            claim(&dir, &served, 999_000).unwrap(),
            (Vec::new(), Some(1_000_000))
        );
        let (due, next) = claim(&dir, &served, missed).unwrap();
        assert_eq!(next, None);
        deliver(&due[0], missed);
        let taken = inbox::take(session, true);
        assert_eq!(taken.len(), 1, "{taken:?}");
        assert_eq!(taken[0].when, When::Later);
        let (first, prompt) = taken[0].text.split_once('\n').unwrap();
        assert!(first.starts_with("wake-up: check CI (due "), "{first}");
        assert!(first.ends_with(", 42 min late)"), "{first}");
        assert_eq!(prompt, "Look at the last run.");
        // It went out of the file as it fired, so it never fires again.
        assert!(!path(&dir).exists());
        assert_eq!(
            claim(&dir, &served, missed + 3_600).unwrap(),
            (Vec::new(), None)
        );
        assert_eq!(late_by(5 * 60 * 60 + 100), "5 h late");
    }

    #[test]
    fn two_letronnas_never_fire_the_same_wake() {
        let dir = Scratch::new("wakes-two");
        put(&dir, wake(931, 100, "check CI")).unwrap();
        put(&dir, wake(932, 100, "the deploy")).unwrap();
        // Each claim opens the lock afresh, as another process does.
        let start = Arc::new(Barrier::new(8));
        let claims: Vec<_> = (0..8)
            .map(|_| {
                let (dir, start) = (dir.to_path_buf(), Arc::clone(&start));
                std::thread::spawn(move || {
                    start.wait();
                    claim(&dir, &BTreeSet::from([931, 932]), 200).unwrap().0
                })
            })
            .collect();
        let mut fired: Vec<u64> = claims
            .into_iter()
            .flat_map(|claim| claim.join().unwrap())
            .map(|wake| wake.session)
            .collect();
        fired.sort_unstable();
        assert_eq!(fired, [931, 932]);
    }

    #[test]
    fn a_wake_more_than_a_day_late_leaves_a_note() {
        let _bell = inbox::tests::DOORBELL
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let session = 941;
        deliver(
            &wake(session, 1_000_000, "check CI"),
            1_000_000 + STALE + 60,
        );
        // No turn starts for it...
        assert!(!inbox::pending().contains(&session));
        assert!(inbox::take(session, false).is_empty());
        // ...but the next one that starts anyway reads it, in a line.
        let taken = inbox::take(session, true);
        assert_eq!(taken.len(), 1, "{taken:?}");
        assert!(!taken[0].text.contains('\n'), "{}", taken[0].text);
        assert!(
            taken[0].text.starts_with("wake-up \"check CI\", due "),
            "{}",
            taken[0].text
        );
        assert!(taken[0].text.contains("more than a day late"));
        // A day late to the second still starts one.
        deliver(&wake(session, 1_000_000, "check CI"), 1_000_000 + STALE);
        assert!(inbox::pending().contains(&session));
        assert!(inbox::take(session, true)[0].text.contains(" h late)\n"));
    }

    #[test]
    fn a_user_message_does_not_cancel_a_wake() {
        let (_data, _bell, dir) = data_folder("wakes-user");
        inbox::listen();
        let session = 951;
        let ctx = ToolContext {
            conversation: session,
            ..Default::default()
        };
        let said = run(
            "wake_me",
            r#"{"seconds": 86400, "reason": "check CI", "prompt": "Look at last night's run."}"#,
            &ctx,
        )
        .unwrap();
        assert!(said.contains(", in 24 hours (\"check CI\")"), "{said}");
        let kept = load(&dir).unwrap();
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].due - kept[0].armed, 86_400);
        assert_eq!(kept[0].prompt, "Look at last night's run.");
        assert!(served().sessions.contains(&session));
        // The user's own turn in the session meanwhile leaves it waiting.
        assert_eq!(turn_ended(session, Some("Done.")), None);
        assert_eq!(load(&dir).unwrap(), kept);
        unsafe { std::env::remove_var("AGENT_DATA_DIR") };
    }

    #[test]
    fn serving_a_session_fires_its_missed_wake_and_keeps_looking() {
        let (_data, _bell, dir) = data_folder("wakes-serve");
        inbox::listen();
        let (first, second) = (961, 962);
        // Both came due while nothing served them, as over a restart.
        let missed = backend::now() - 5 * 60;
        put(&dir, wake(first, missed, "check CI")).unwrap();
        put(&dir, wake(second, missed, "the deploy")).unwrap();
        serve([first]);
        let armed = served().timer;
        // The look runs on a thread of its own a moment later.
        let deadline = Instant::now() + Duration::from_secs(5);
        while !inbox::pending().contains(&first) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        let taken = inbox::take(first, true);
        assert_eq!(taken.len(), 1, "{taken:?}");
        let (line, _) = taken[0].text.split_once('\n').unwrap();
        assert!(line.starts_with("wake-up: check CI (due "), "{line}");
        assert!(line.ends_with(", 5 min late)"), "{line}");
        // The session it does not serve keeps its wake-up in the file.
        let left: Vec<u64> = load(&dir).unwrap().iter().map(|w| w.session).collect();
        assert_eq!(left, [second]);
        // With none of its own waiting it still looks again, for one that
        // another Letronna leaves meanwhile.
        while served().timer == armed && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert_ne!(served().timer, armed);
        serve([]);
        unsafe { std::env::remove_var("AGENT_DATA_DIR") };
    }

    #[test]
    fn a_wake_a_day_late_stops_the_loop_waiting_on_it() {
        let (_data, _bell, dir) = data_folder("wakes-stale-loop");
        inbox::listen();
        let session = 942;
        command(session, "/loop", "watch the deploy")
            .unwrap()
            .unwrap();
        take_tick(session);
        let ctx = ToolContext {
            conversation: session,
            ..Default::default()
        };
        run(
            "wake_me",
            r#"{"seconds": 86400, "reason": "rollout"}"#,
            &ctx,
        )
        .unwrap();
        assert_eq!(turn_ended(session, Some("Back tomorrow.")), None);
        // A laptop shut for two days finds it a day late once it opens.
        let opened = load(&dir).unwrap()[0].due + STALE + 60;
        let (woken, _) = claim(&dir, &BTreeSet::from([session]), opened).unwrap();
        deliver(&woken[0], opened);
        assert!(looping(session).is_none());
        let taken = inbox::take(session, true);
        assert_eq!(taken.len(), 1, "{taken:?}");
        assert_eq!(taken[0].when, When::Quiet);
        assert!(
            taken[0]
                .text
                .ends_with("started no turn and stopped the /loop waiting on it."),
            "{}",
            taken[0].text
        );
        unsafe { std::env::remove_var("AGENT_DATA_DIR") };
    }

    #[test]
    fn a_wake_that_cannot_be_locked_says_so() {
        let dir = Scratch::new("wakes-unlocked");
        // A file where the data folder should be leaves no room for the lock.
        let file = dir.join("data");
        std::fs::write(&file, "").unwrap();
        let refused = put(&file, wake(971, 100, "check CI")).unwrap_err();
        assert!(
            refused.starts_with("Could not lock the wake-ups: "),
            "{refused}"
        );
    }
}
