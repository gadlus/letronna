//! `wake_me`: end the turn now and have the session woken later.
//!
//! Coming back in twenty minutes used to mean holding the turn open in
//! `wait`, which keeps a round in flight and gives up after two minutes. A
//! wake-up is instead one deadline on the shared timer thread that leaves a
//! `Later` notice in the session's mailbox, so nothing runs while the session
//! sleeps and the front end that listens starts its next turn with the
//! notice. A session has one at a time, or a model that arms one every turn
//! would be woken once for each. It lives in memory only: a pending wake-up
//! is lost when Letronna exits, and outliving a restart is a schedule's job.
//!
//! `/loop` and `/heartbeat` keep a session going on the same footing. A loop
//! leaves its prompt in the mailbox as a tick, the first at once and each
//! after that an interval on from the end of the last, or, with no interval,
//! when the model's own wake-up comes due. Only the end of the turn that took
//! a tick can say whether it answered LOOP_COMPLETE or asked to be woken, so
//! the front ends report every turn's end and the loop picks out its own. A
//! session has one loop, held in memory like its wake-up.

use crate::chat::Connection;
use crate::inbox::{self, Notice, When};
use crate::timers::{self, TimerId};
use crate::tools::{Tool, ToolContext, one_line};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

/// A minute: anything sooner is a pause within the turn, which `wait` keeps
/// without ending it.
const SOONEST: f64 = 60.;

/// An hour: a longer sleep belongs to a schedule, which outlives a restart
/// where a wake-up does not.
const LATEST: f64 = 3_600.;

/// What a woken session is told to do when the model named nothing else.
const CARRY_ON: &str = "Carry on where you left off.";

/// The wake-up each session has pending, so a newer one can take it back.
static PENDING: Mutex<BTreeMap<u64, TimerId>> = Mutex::new(BTreeMap::new());

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
            "seconds": {"type": "integer", "description": "60 to 3600."},
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
            .ok_or("`seconds` is required: how long to sleep, 60 to 3600.")?;
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
        let text = format!("wake-up: {reason}\n{prompt}");
        arm(
            session,
            delay,
            Box::new(move || {
                if wake_loop(session, &text) {
                    return;
                }
                inbox::push(
                    session,
                    Notice {
                        from: "wake-up".into(),
                        when: When::Later,
                        text,
                    },
                );
            }),
        );
        asked_to_wake(session);
        let at = (chrono::Local::now() + delay).format("%H:%M");
        let minutes = (delay.as_secs() + 30) / 60;
        let s = if minutes == 1 { "" } else { "s" };
        Ok(format!(
            "This session wakes at {at}, in {minutes} minute{s} (\"{reason}\"). End your turn now. A newer wake_me replaces this one."
        ))
    }
}

/// Whether a session is woken once its turn ends, which takes a listening
/// front end and no chat app, whose answer would have nowhere to go.
fn woken(listened: bool, connection: &Connection) -> bool {
    listened && connection.remote.is_none()
}

/// The sleep asked for, held to between a minute and an hour.
fn delay(seconds: f64) -> Duration {
    Duration::from_secs_f64(seconds.clamp(SOONEST, LATEST))
}

/// Run `fire` for a session once `delay` has passed, taking back the wake-up
/// it had pending.
fn arm(session: u64, delay: Duration, fire: Box<dyn FnOnce() + Send>) {
    // Held across both steps, so two calls at once still leave one pending.
    let mut pending = PENDING.lock().unwrap_or_else(PoisonError::into_inner);
    let timer = timers::at(Instant::now() + delay, fire);
    // One that already fired stays listed until the next takes it back,
    // which does nothing.
    if let Some(older) = pending.insert(session, timer) {
        timers::cancel(older);
    }
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
        if self.pace == Pace::Model
            && let Some(timer) = PENDING
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .remove(&session)
        {
            timers::cancel(timer);
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

/// Note that the model asked to be woken, which in a loop it paces makes the
/// fallback needless and forgives a tick that asked for nothing.
fn asked_to_wake(session: u64) {
    let mut loops = loops();
    let Some(looping) = loops.get_mut(&session) else {
        return;
    };
    looping.woke = true;
    looping.missed = false;
    if looping.pace == Pace::Model
        && let Some(timer) = looping.timer.take()
    {
        timers::cancel(timer);
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
    use crate::tools::{run, specs};
    use std::sync::mpsc::{self, RecvTimeoutError, TryRecvError};

    /// A callback that sends `label` down `sent`, and drops `sent` unsent if
    /// it is taken back.
    fn say(sent: mpsc::Sender<&'static str>, label: &'static str) -> Box<dyn FnOnce() + Send> {
        Box::new(move || {
            let _ = sent.send(label);
        })
    }

    #[test]
    fn a_newer_wake_replaces_the_pending_one() {
        let (older, older_fired) = mpsc::channel();
        let (other, other_fired) = mpsc::channel();
        let (newer, newer_fired) = mpsc::channel();
        // An hour off, so the older one can only end by being taken back.
        arm(901, Duration::from_secs(3_600), say(older, "older"));
        arm(902, Duration::from_secs(3_600), say(other, "other"));
        arm(901, Duration::from_millis(50), say(newer, "newer"));
        assert_eq!(
            older_fired.recv_timeout(Duration::from_secs(10)),
            Err(RecvTimeoutError::Disconnected)
        );
        assert_eq!(
            newer_fired.recv_timeout(Duration::from_secs(10)),
            Ok("newer")
        );
        // Another session's wake-up is its own, and still pending.
        assert_eq!(other_fired.try_recv(), Err(TryRecvError::Empty));
        timers::cancel(PENDING.lock().unwrap()[&902]);
    }

    #[test]
    fn delays_are_clamped_to_a_minute_and_an_hour() {
        for (asked, slept) in [
            (-5., 60),
            (0., 60),
            (5., 60),
            (90., 90),
            (1_200., 1_200),
            (86_400., 3_600),
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
        assert!(!PENDING.lock().unwrap().contains_key(&904));
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
        let _bell = inbox::tests::DOORBELL
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        inbox::listen();
        let session = 912;
        command(session, "/loop", "watch the deploy")
            .unwrap()
            .unwrap();
        assert!(take_tick(session).contains("call wake_me"));

        // A tick that asked to be woken has no fallback: its wake-up is the
        // next tick.
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
        assert!(loops()[&session].timer.is_none());
        let wake_up = PENDING.lock().unwrap().remove(&session).unwrap();
        timers::cancel(wake_up);
        assert!(wake_loop(
            session,
            &format!("wake-up: rollout at 40%\n{CARRY_ON}")
        ));
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
}
