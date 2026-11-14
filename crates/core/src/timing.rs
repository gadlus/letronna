//! Where a run's time went: waiting on the model, running tools, and in the
//! driver's own checks.
//!
//! A run's result said only how long it took in all, so nothing could tell a
//! slow provider from slow tools, or say how long a reply took to begin.
//! The driver notes each request and each call here as it ends, by the
//! session the work is for, and the result line of a run nobody watches
//! takes the totals with it. What the driver does itself between a run's
//! replies is neither: the reviews it starts and waits for, the tester's
//! tests it runs against the work, a second attempt, and the project's
//! tests run as they were. Those are noted apart, so a run that spent ten
//! minutes there does not read as ten minutes nobody can account for.

use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock, PoisonError};
use std::time::Duration;

/// What one agent, or a run's helpers taken together, spent waiting.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Spent {
    /// Requests to the model that came to an end, a retried one once.
    pub requests: u32,
    /// From sending each to the end of its reply, its retries and their
    /// rests included.
    pub model: Duration,
    /// The requests whose reply began: wrote, thought or started a call.
    pub begun: u32,
    /// From sending those to that beginning, added up.
    pub first: Duration,
    /// The longest of them.
    pub slowest_first: Duration,
    /// Waiting for a place on the provider before a request went.
    pub queued: Duration,
    /// Tool calls run.
    pub calls: u32,
    /// The time they took by the clock: calls run at once count the time
    /// of their batch, not of each.
    pub tools: Duration,
}

/// A run's time: its own agent's, its helpers', and the driver's checks. A
/// helper the agent waited for is also inside the agent's own tool time, as
/// the call that started it took that long; one the driver started for a
/// check is inside the checks' time instead.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Timing {
    pub own: Spent,
    pub helpers: Spent,
    /// The driver's own checks by the clock, from starting each to its end.
    pub checks: Duration,
}

fn runs() -> &'static Mutex<HashMap<u64, Timing>> {
    static RUNS: OnceLock<Mutex<HashMap<u64, Timing>>> = OnceLock::new();
    RUNS.get_or_init(Default::default)
}

fn note(root: u64, helper: bool, add: impl FnOnce(&mut Spent)) {
    let mut runs = runs().lock().unwrap_or_else(PoisonError::into_inner);
    let timing = runs.entry(root).or_default();
    add(match helper {
        true => &mut timing.helpers,
        false => &mut timing.own,
    });
}

/// A request for session `root`, its own agent's or a `helper`'s, came to
/// an end `whole` after it was sent, having `queued` for a place first; its
/// reply began after `first`, if it began at all.
pub(crate) fn asked(
    root: u64,
    helper: bool,
    queued: Duration,
    first: Option<Duration>,
    whole: Duration,
) {
    note(root, helper, |spent| {
        spent.requests += 1;
        spent.model += whole;
        spent.queued += queued;
        if let Some(first) = first {
            spent.begun += 1;
            spent.first += first;
            spent.slowest_first = spent.slowest_first.max(first);
        }
    });
}

/// `calls` tool calls for session `root` ran, at once if more than one, and
/// `took` that long together.
pub(crate) fn ran(root: u64, helper: bool, calls: usize, took: Duration) {
    note(root, helper, |spent| {
        spent.calls += u32::try_from(calls).unwrap_or(u32::MAX);
        spent.tools += took;
    });
}

/// A check the driver made itself for session `root` took `took`.
pub(crate) fn checked(root: u64, took: Duration) {
    let mut runs = runs().lock().unwrap_or_else(PoisonError::into_inner);
    runs.entry(root).or_default().checks += took;
}

/// What session `root` has spent since this was last asked, which starts
/// its count again: each result line says the time of the run it ends.
pub fn take(root: u64) -> Timing {
    let mut runs = runs().lock().unwrap_or_else(PoisonError::into_inner);
    runs.remove(&root).unwrap_or_default()
}

/// Seconds to the millisecond, as a result line says them.
fn seconds(time: Duration) -> f64 {
    time.as_millis() as f64 / 1000.0
}

impl Spent {
    fn json(&self) -> Value {
        json!({
            "requests": self.requests,
            "model_seconds": seconds(self.model),
            "first_token": {
                "count": self.begun,
                "seconds": seconds(self.first),
                "slowest_seconds": seconds(self.slowest_first),
            },
            "queued_seconds": seconds(self.queued),
            "tool_calls": self.calls,
            "tool_seconds": seconds(self.tools),
        })
    }
}

impl Timing {
    /// As `--output json` says it under `timing`: the run's own figures,
    /// the seconds of the driver's checks, and `helpers` with the run's
    /// figures for them when any started.
    pub fn json(&self) -> Value {
        let mut out = self.own.json();
        out["checks_seconds"] = json!(seconds(self.checks));
        if self.helpers != Spent::default() {
            out["helpers"] = self.helpers.json();
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MS: fn(u64) -> Duration = Duration::from_millis;

    #[test]
    fn a_runs_requests_and_calls_add_up_apart_from_its_helpers() {
        // An id no other test's session has.
        let root = 0x71_4d_1e_01;
        asked(root, false, MS(40), Some(MS(900)), MS(2_500));
        asked(root, false, MS(0), Some(MS(1_400)), MS(3_000));
        // A reply a stop ended before it began has no first token.
        asked(root, false, MS(10), None, MS(200));
        ran(root, false, 3, MS(750));
        ran(root, false, 1, MS(250));
        asked(root, true, MS(0), Some(MS(300)), MS(1_000));
        ran(root, true, 2, MS(100));
        checked(root, MS(4_000));
        checked(root, MS(2_500));

        let timing = take(root);
        assert_eq!(
            timing.own,
            Spent {
                requests: 3,
                model: MS(5_700),
                begun: 2,
                first: MS(2_300),
                slowest_first: MS(1_400),
                queued: MS(50),
                calls: 4,
                tools: MS(1_000),
            }
        );
        assert_eq!((timing.helpers.requests, timing.helpers.calls), (1, 2));
        assert_eq!(timing.checks, MS(6_500));
        assert_eq!(
            timing.json(),
            json!({
                "requests": 3,
                "model_seconds": 5.7,
                "first_token": {"count": 2, "seconds": 2.3, "slowest_seconds": 1.4},
                "queued_seconds": 0.05,
                "tool_calls": 4,
                "tool_seconds": 1.0,
                "checks_seconds": 6.5,
                "helpers": {
                    "requests": 1,
                    "model_seconds": 1.0,
                    "first_token": {"count": 1, "seconds": 0.3, "slowest_seconds": 0.3},
                    "queued_seconds": 0.0,
                    "tool_calls": 2,
                    "tool_seconds": 0.1,
                },
            })
        );
        // Taken, so the next run of the session starts from nothing.
        assert_eq!(take(root), Timing::default());
        assert!(take(root).json().get("helpers").is_none());
    }
}
