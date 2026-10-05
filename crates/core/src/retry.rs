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
/// How often a wait looks to see whether the turn was stopped.
const SLICE: Duration = Duration::from_millis(100);

/// What a frontend that cannot take back the words of a cut reply says
/// after them.
pub const DROPPED: &str =
    "The provider closed the stream early, so that reply is dropped and asked for again.";

/// A failed call about to be made again, as the frontends are told of it.
#[derive(Clone, Copy, Debug, PartialEq)]
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
}

impl fmt::Display for Retry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let why = match self.kind {
            FailureKind::RateLimit => "Rate limited",
            FailureKind::Overloaded => "The provider is overloaded",
            FailureKind::Network if self.dropped => "The provider closed the stream early",
            FailureKind::Network => "The connection failed",
            _ => "The provider had an error",
        };
        let seconds = self.wait.as_millis().div_ceil(1000);
        write!(
            f,
            "{why}, trying again in {seconds}s (try {} of {})",
            self.attempt, self.of
        )
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
/// `None` when it is not worth making.
pub fn after(failure: &Failure, tried: u32, unattended: bool) -> Option<Retry> {
    let (of, patience) = if unattended {
        (TRIES_UNATTENDED, PATIENCE_UNATTENDED)
    } else {
        (TRIES, PATIENCE)
    };
    if !passes(failure.kind) || tried >= of {
        return None;
    }
    let wait = backoff(tried, jitter()).max(failure.retry_after.unwrap_or_default());
    (wait <= patience).then_some(Retry {
        kind: failure.kind,
        attempt: tried + 1,
        of,
        wait,
        dropped: false,
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
            assert!(after(&failure(kind, None), 1, false).is_some(), "{kind:?}");
        }
        for kind in never {
            assert!(after(&failure(kind, None), 1, false).is_none(), "{kind:?}");
            assert!(after(&failure(kind, None), 1, true).is_none(), "{kind:?}");
        }
    }

    #[test]
    fn six_tries_while_watched_and_twenty_when_not() {
        let overloaded = failure(FailureKind::Overloaded, None);
        let next = after(&overloaded, 5, false).unwrap();
        assert_eq!((next.attempt, next.of), (6, 6));
        assert!(after(&overloaded, 6, false).is_none());
        let next = after(&overloaded, 19, true).unwrap();
        assert_eq!((next.attempt, next.of), (20, 20));
        assert!(after(&overloaded, 20, true).is_none());
    }

    #[test]
    fn retry_after_is_the_shortest_wait() {
        let asked = Duration::from_secs(20);
        let next = after(&failure(FailureKind::RateLimit, Some(asked)), 1, false).unwrap();
        assert_eq!(next.wait, asked);
        // A shorter one leaves the doubling as it is.
        let next = after(
            &failure(FailureKind::RateLimit, Some(Duration::ZERO)),
            4,
            false,
        )
        .unwrap();
        assert!(next.wait >= Duration::from_secs(4), "{:?}", next.wait);
    }

    #[test]
    fn a_wait_over_a_minute_gives_up_at_once_unless_nobody_is_watching() {
        let long = failure(FailureKind::RateLimit, Some(Duration::from_secs(61)));
        assert!(after(&long, 1, false).is_none());
        assert_eq!(after(&long, 1, true).unwrap().wait, Duration::from_secs(61));
        let longer = failure(FailureKind::RateLimit, Some(Duration::from_secs(3_600)));
        assert!(after(&longer, 1, true).is_none());
    }

    #[test]
    fn the_retry_is_told_in_words() {
        let retry = Retry {
            kind: FailureKind::RateLimit,
            attempt: 2,
            of: 6,
            wait: Duration::from_millis(1_100),
            dropped: false,
        };
        assert_eq!(
            retry.to_string(),
            "Rate limited, trying again in 2s (try 2 of 6)"
        );
        let cut = Retry {
            kind: FailureKind::Network,
            dropped: true,
            ..retry
        };
        assert_eq!(
            cut.to_string(),
            "The provider closed the stream early, trying again in 2s (try 2 of 6)"
        );
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
