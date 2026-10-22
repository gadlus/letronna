//! Trying a failed model call again, when the failure is one that passes.
//!
//! A rate limit, an overload, a server error or a dropped connection is the
//! commonest way a call fails on a shared gateway, and each goes away on its
//! own; with one short retry, each of them ended an agent task part way
//! through. Those kinds are now tried again, waiting twice as long each time
//! from half a second up to 32, plus a little jitter so the window and the
//! terminal do not come back in step, and never sooner than the provider's
//! `Retry-After`. Someone watching would rather be told than sit through a
//! wait of minutes, so a longer one gives up at once; a schedule or a chat
//! app, which nobody may be watching, gets more tries and a longer wait.
//! One of those that must end by a deadline, as a benchmark's run does,
//! keeps trying past them while a wait still ends before it: a gateway down
//! for longer than eight minutes would otherwise end the run with most of
//! its time unused, and giving up gains it nothing. Such a run does not give
//! up on a gateway that asks it to come back in an hour either, as one with
//! no provider for the model does: it keeps asking at its own pace, though a
//! rate limit's wait, a quota's, is still kept.
//! A reply whose stream is cut part way is tried again the same way, and
//! what it had said so far is dropped, never kept or run as if whole.
//!
//! A wait sleeps in short slices and ends as soon as the frontend stops
//! listening, which is how both of them stop a turn.

use crate::failure::{Failure, FailureKind};
use std::{
    fmt,
    hash::{BuildHasher, Hasher, RandomState},
    time::{Duration, Instant},
};

/// Tries in all while someone is watching: the waits before the last add up
/// to about fifteen seconds, about as long as anyone watches a spinner.
const TRIES: u32 = 6;
/// Tries in all when nobody may be watching: once the doubling stops, one
/// every half minute or so, for eight minutes or so in all.
const TRIES_UNATTENDED: u32 = 20;
/// The first wait, doubled for each try after it.
const FIRST_WAIT: Duration = Duration::from_millis(500);
/// Where the doubling stops, reached by the wait before the eighth try.
const LONGEST_WAIT: Duration = Duration::from_secs(32);
/// The longest wait put on someone watching: past a minute they would rather
/// be told, and can try again themselves.
const PATIENCE: Duration = Duration::from_secs(60);
/// The longest wait when nobody may be watching. A wait of more than a few
/// minutes is a limit for the hour or the day, better reported than slept
/// through while every other schedule queues behind the run.
const PATIENCE_UNATTENDED: Duration = Duration::from_secs(10 * 60);
/// How long before its deadline a run nobody watches stops trying again
/// past its usual tries, so the wait never runs into the end of the run.
const DEADLINE_MARGIN: Duration = Duration::from_secs(60);
/// How often a wait looks to see whether the turn was stopped.
const SLICE: Duration = Duration::from_millis(100);

/// What a frontend that cannot take back the words of a cut reply says
/// after them.
pub const DROPPED: &str =
    "The provider closed the stream early, so that reply is dropped and asked for again.";

/// A failed call about to be made again, as the frontends are told of it.
#[derive(Clone, Debug, PartialEq)]
pub struct Retry {
    /// Why the last try failed.
    pub kind: FailureKind,
    /// The try about to be made, the first having been 1.
    pub attempt: u32,
    /// The most tries there will be.
    pub of: u32,
    /// How long until it is made.
    pub wait: Duration,
    /// The last try broke off part way through its reply, so what it said
    /// is dropped and the next try's reply comes in its place.
    pub dropped: bool,
    /// What the provider said on the last try, as its failure words it. A
    /// kind alone leaves whoever reads the line, or the log, to guess why.
    pub error: String,
}

impl Retry {
    /// What is being waited for, without the provider's words: the window
    /// shows those on a line of their own, where a long one can wrap.
    pub fn waiting(&self) -> String {
        // Only a reply cut off before it wrote anything is asked for again
        // after a bad reply, at once and with a longer limit.
        if self.kind == FailureKind::BadReply {
            return format!(
                "The model spent its whole reply thinking, asking again with more room (try {} of {})",
                self.attempt, self.of
            );
        }
        let why = match self.kind {
            FailureKind::RateLimit => "Rate limited",
            FailureKind::Overloaded => "The provider is overloaded",
            FailureKind::Network if self.dropped => "The provider closed the stream early",
            FailureKind::Network => "The connection failed",
            _ => "The provider had an error",
        };
        let seconds = self.wait.as_millis().div_ceil(1000);
        format!(
            "{why}, trying again in {seconds}s (try {} of {})",
            self.attempt, self.of
        )
    }

    /// The provider's words, on one line; empty after a bad reply, whose
    /// error is Letronna's own and says no more than `waiting` does.
    pub fn said(&self) -> String {
        if self.kind == FailureKind::BadReply {
            return String::new();
        }
        crate::tools::one_line(&self.error)
    }
}

impl fmt::Display for Retry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.waiting())?;
        match self.said() {
            said if said.is_empty() => Ok(()),
            said => write!(f, ": {said}"),
        }
    }
}

/// Whether a failure of this kind can pass by itself, so the same request
/// may yet succeed.
pub fn passes(kind: FailureKind) -> bool {
    matches!(
        kind,
        FailureKind::RateLimit
            | FailureKind::Overloaded
            | FailureKind::Server
            | FailureKind::Network
    )
}

/// The try to make after `tried` of them failed, the last with `failure`, or
/// `None` when it is not worth making. A run nobody watches that must end
/// by `deadline` goes on past its usual tries while the wait still ends
/// `DEADLINE_MARGIN` before it.
pub fn after(
    failure: &Failure,
    tried: u32,
    unattended: bool,
    deadline: Option<Instant>,
) -> Option<Retry> {
    let (of, patience) = if unattended {
        (TRIES_UNATTENDED, PATIENCE_UNATTENDED)
    } else {
        (TRIES, PATIENCE)
    };
    if !passes(failure.kind) || failure.lasting {
        return None;
    }
    let mut wait = backoff(tried, jitter()).max(failure.retry_after.unwrap_or_default());
    let timed = deadline.filter(|_| unattended);
    if wait > patience {
        // A gateway with no provider for the model asks for an hour or more,
        // and a run that must end by a deadline would lose the rest of its
        // time by giving up, so it keeps asking at its own pace. A rate
        // limit's wait is a quota's, so it is kept.
        if timed.is_none() || failure.kind == FailureKind::RateLimit {
            return None;
        }
        wait = backoff(tried, jitter());
    }
    if tried < of {
        return Some(Retry {
            kind: failure.kind,
            attempt: tried + 1,
            of,
            wait,
            dropped: false,
            error: failure.to_string(),
        });
    }
    let deadline = timed?;
    let left =
        (deadline.saturating_duration_since(Instant::now())).checked_sub(wait + DEADLINE_MARGIN)?;
    // The tries still to come at the longest wait, so the count the
    // frontends show stays ahead of the try being made.
    let more = u32::try_from(left.as_secs() / LONGEST_WAIT.as_secs()).unwrap_or(u32::MAX);
    Some(Retry {
        kind: failure.kind,
        attempt: tried + 1,
        of: (tried + 1).saturating_add(more),
        wait,
        dropped: false,
        error: failure.to_string(),
    })
}

/// The wait after `tried` failed tries, before any `Retry-After`, with
/// `jitter` from 0 to 1 adding up to a quarter more.
fn backoff(tried: u32, jitter: f64) -> Duration {
    let doubled = FIRST_WAIT.saturating_mul(2u32.saturating_pow(tried.saturating_sub(1)));
    doubled
        .min(LONGEST_WAIT)
        .mul_f64(1.0 + jitter.clamp(0.0, 1.0) / 4.0)
}

/// A number from 0 up to 1 that differs from one call to the next.
fn jitter() -> f64 {
    // Each RandomState is keyed afresh, which is all the chance needed to
    // keep two frontends from coming back at the same moment.
    let bits = RandomState::new().build_hasher().finish();
    (bits >> 11) as f64 / (1u64 << 53) as f64
}

/// Sleep for `wait` unless `stopped` turns true first; false when it did.
pub fn pause(wait: Duration, stopped: impl Fn() -> bool) -> bool {
    let until = Instant::now() + wait;
    while !stopped() {
        let left = until.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return true;
        }
        std::thread::sleep(left.min(SLICE));
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    /// A failure of `kind` that asked for `retry_after`.
    fn failure(kind: FailureKind, retry_after: Option<Duration>) -> Failure {
        let mut failure = Failure::new(kind, "It failed.");
        failure.retry_after = retry_after;
        failure
    }

    #[test]
    fn the_wait_doubles_from_half_a_second_up_to_32() {
        let waits: Vec<u128> = (1..=9)
            .map(|tried| backoff(tried, 0.0).as_millis())
            .collect();
        assert_eq!(
            waits,
            [
                500, 1_000, 2_000, 4_000, 8_000, 16_000, 32_000, 32_000, 32_000
            ]
        );
        // Far past the cap nothing overflows.
        assert_eq!(backoff(u32::MAX, 0.0), LONGEST_WAIT);
    }

    #[test]
    fn jitter_adds_at_most_a_quarter() {
        assert_eq!(backoff(3, 1.0), Duration::from_millis(2_500));
        for _ in 0..100 {
            let wait = backoff(2, jitter());
            assert!(
                (Duration::from_secs(1)..=Duration::from_millis(1_250)).contains(&wait),
                "{wait:?}"
            );
        }
    }

    #[test]
    fn only_failures_that_pass_by_themselves_are_tried_again() {
        let again = [
            FailureKind::RateLimit,
            FailureKind::Overloaded,
            FailureKind::Server,
            FailureKind::Network,
        ];
        let never = [
            FailureKind::Auth,
            FailureKind::Quota,
            FailureKind::ContextTooLong,
            FailureKind::BadRequest,
            FailureKind::NotFound,
            FailureKind::BadReply,
            FailureKind::Settings,
            FailureKind::Stopped,
            FailureKind::Unknown,
        ];
        for kind in again {
            assert!(
                after(&failure(kind, None), 1, false, None).is_some(),
                "{kind:?}"
            );
        }
        for kind in never {
            assert!(
                after(&failure(kind, None), 1, false, None).is_none(),
                "{kind:?}"
            );
            assert!(
                after(&failure(kind, None), 1, true, None).is_none(),
                "{kind:?}"
            );
        }
    }

    #[test]
    fn six_tries_while_watched_and_twenty_when_not() {
        let overloaded = failure(FailureKind::Overloaded, None);
        let next = after(&overloaded, 5, false, None).unwrap();
        assert_eq!((next.attempt, next.of), (6, 6));
        assert!(after(&overloaded, 6, false, None).is_none());
        let next = after(&overloaded, 19, true, None).unwrap();
        assert_eq!((next.attempt, next.of), (20, 20));
        assert!(after(&overloaded, 20, true, None).is_none());
    }

    #[test]
    fn a_run_with_a_deadline_keeps_trying_while_a_wait_ends_before_it() {
        let overloaded = failure(FailureKind::Overloaded, None);
        let later = Instant::now() + Duration::from_secs(10 * 60);
        let next = after(&overloaded, 20, true, Some(later)).unwrap();
        assert_eq!(next.attempt, 21);
        assert!(next.of > 21, "{}", next.of);
        let next = after(&overloaded, 40, true, Some(later)).unwrap();
        assert!(next.of > 41, "{}", next.of);
        // Not once the wait would reach the last minute, nor while watched.
        let soon = Instant::now() + Duration::from_secs(90);
        assert!(after(&overloaded, 20, true, Some(soon)).is_none());
        assert!(after(&overloaded, 6, false, Some(later)).is_none());
        // A gateway's hour-long Retry-After is not slept through but not
        // given up on either; a rate limit's is still kept.
        let mut down = failure(FailureKind::Overloaded, Some(Duration::from_secs(3_788)));
        let next = after(&down, 1, true, Some(later)).unwrap();
        assert!(next.wait <= Duration::from_secs(40), "{:?}", next.wait);
        assert!(after(&down, 1, true, None).is_none());
        assert!(after(&down, 1, false, Some(later)).is_none());
        down.kind = FailureKind::RateLimit;
        assert!(after(&down, 1, true, Some(later)).is_none());
        // What never passes is never waited out, deadline or not.
        let refused = failure(FailureKind::Auth, None);
        assert!(after(&refused, 1, true, Some(later)).is_none());
    }

    #[test]
    fn retry_after_is_the_shortest_wait() {
        let asked = Duration::from_secs(20);
        let next = after(
            &failure(FailureKind::RateLimit, Some(asked)),
            1,
            false,
            None,
        )
        .unwrap();
        assert_eq!(next.wait, asked);
        // A shorter one leaves the doubling as it is.
        let next = after(
            &failure(FailureKind::RateLimit, Some(Duration::ZERO)),
            4,
            false,
            None,
        )
        .unwrap();
        assert!(next.wait >= Duration::from_secs(4), "{:?}", next.wait);
    }

    #[test]
    fn a_wait_over_a_minute_gives_up_at_once_unless_nobody_is_watching() {
        let long = failure(FailureKind::RateLimit, Some(Duration::from_secs(61)));
        assert!(after(&long, 1, false, None).is_none());
        assert_eq!(
            after(&long, 1, true, None).unwrap().wait,
            Duration::from_secs(61)
        );
        let longer = failure(FailureKind::RateLimit, Some(Duration::from_secs(3_600)));
        assert!(after(&longer, 1, true, None).is_none());
    }

    #[test]
    fn the_retry_is_told_in_words() {
        let retry = Retry {
            kind: FailureKind::RateLimit,
            attempt: 2,
            of: 6,
            wait: Duration::from_millis(1_100),
            dropped: false,
            error: String::new(),
        };
        assert_eq!(
            retry.to_string(),
            "Rate limited, trying again in 2s (try 2 of 6)"
        );
        let cut = Retry {
            kind: FailureKind::Network,
            dropped: true,
            ..retry.clone()
        };
        assert_eq!(
            cut.to_string(),
            "The provider closed the stream early, trying again in 2s (try 2 of 6)"
        );
        let roomier = Retry {
            kind: FailureKind::BadReply,
            error: "The model reached its output limit before writing anything.".into(),
            ..retry
        };
        assert_eq!(
            roomier.to_string(),
            "The model spent its whole reply thinking, asking again with more room (try 2 of 6)"
        );
    }

    #[test]
    fn the_retry_says_what_the_provider_said() {
        let refused = failure(FailureKind::Server, Some(Duration::from_secs(9)));
        let mut retry = after(&refused, 3, false, None).unwrap();
        assert_eq!(retry.error, "It failed.");
        assert_eq!(
            retry.to_string(),
            "The provider had an error, trying again in 9s (try 4 of 6): It failed."
        );
        // Words over several lines still fit the one row they are shown in.
        retry.error = "upstream connect error\n  reset reason: overflow\n".into();
        assert_eq!(
            retry.to_string(),
            "The provider had an error, trying again in 9s (try 4 of 6): \
             upstream connect error reset reason: overflow"
        );
        // The window shows the two apart, so a long error can wrap.
        assert_eq!(
            retry.waiting(),
            "The provider had an error, trying again in 9s (try 4 of 6)"
        );
        assert_eq!(
            retry.said(),
            "upstream connect error reset reason: overflow"
        );
        // A reply spent thinking is told by the wait alone.
        retry.kind = FailureKind::BadReply;
        assert!(retry.said().is_empty() && !retry.to_string().contains("overflow"));
    }

    #[test]
    fn a_wait_ends_as_soon_as_the_turn_is_stopped() {
        // Long enough to hang the test if the stop were missed.
        assert!(!pause(Duration::from_secs(600), || true));
        let looks = Cell::new(0);
        let stopped = pause(Duration::from_secs(600), || {
            looks.set(looks.get() + 1);
            looks.get() > 2
        });
        assert!(!stopped);
        assert_eq!(looks.get(), 3);
        assert!(pause(Duration::from_millis(20), || false));
    }
}
