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

use crate::chat::Connection;
use crate::inbox::{self, Notice, When};
use crate::timers::{self, TimerId};
use crate::tools::{Tool, ToolContext, one_line};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::sync::{Mutex, PoisonError};
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
}
