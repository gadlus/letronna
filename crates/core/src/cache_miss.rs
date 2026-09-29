//! Why a provider's prompt cache stopped serving a session's requests.
//!
//! A cache hit needs the start of a request to match the one before it byte
//! for byte, so when the cached share of a reply falls away, the part of
//! that start which changed is the likely reason. Each request built for a
//! session keeps a hash of each part, in the order the prefix holds them,
//! and the first one that differs from the last answered request's is named;
//! with nothing changed, a pause longer than the cache lives names the time,
//! and otherwise the provider let it go for reasons of its own.
//!
//! Only hashes are kept, only in memory, and only for this run: a miss is a
//! question about now, and the reason found rides on the reply's usage.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::sync::{Mutex, OnceLock};

/// Seconds a provider keeps an unused prefix: Anthropic's and OpenAI's
/// default is about five minutes.
const LIFETIME: u64 = 5 * 60;
/// Cached tokens below which a drop is not worth naming. Anthropic caches
/// nothing shorter than this, and a small prefix costs little to write again.
const LEAST: u64 = 1_024;
/// Misses `/usage` lists, newest first.
pub const SHOWN: usize = 5;

/// Why a reply was served far less from the cache than the one before it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Reason {
    /// Who Letronna is, its rules, the date or the computer it runs on.
    System,
    Memory,
    Skills,
    /// The summary of the turns the window no longer holds.
    Summary,
    Tools,
    Model,
    /// Nothing changed, but longer passed than the provider keeps a prefix.
    Expired,
    /// Nothing changed in time; the provider let it go on its own. A reason
    /// a later version saved reads as this too.
    #[serde(other)]
    Unknown,
}

impl Reason {
    /// The reason as a line says it.
    pub fn text(self) -> &'static str {
        match self {
            Self::System => "the system prompt changed",
            Self::Memory => "memory changed",
            Self::Skills => "the skills changed",
            Self::Summary => "the summary changed",
            Self::Tools => "the tools changed",
            Self::Model => "the model changed",
            Self::Expired => "over 5 minutes idle",
            Self::Unknown => "nothing changed that Letronna sends",
        }
    }

    /// The reason as the diag log names it.
    pub fn id(self) -> &'static str {
        match self {
            Self::System => "system",
            Self::Memory => "memory",
            Self::Skills => "skills",
            Self::Summary => "summary",
            Self::Tools => "tools",
            Self::Model => "model",
            Self::Expired => "expired",
            Self::Unknown => "unknown",
        }
    }
}

/// The parts of a request a cached prefix covers, in the order it holds them.
const ORDER: [Reason; 6] = [
    Reason::System,
    Reason::Memory,
    Reason::Skills,
    Reason::Summary,
    Reason::Tools,
    Reason::Model,
];

/// A hash of each part of one request's cached prefix, in `ORDER`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Prefix([u64; ORDER.len()]);

impl Prefix {
    /// Keep the hash of `value` as `part`; a reason that is no part is ignored.
    pub(crate) fn set(&mut self, part: Reason, value: impl Hash) {
        let Some(at) = ORDER.iter().position(|&p| p == part) else {
            return;
        };
        let mut hasher = DefaultHasher::new();
        value.hash(&mut hasher);
        self.0[at] = hasher.finish();
    }

    /// The first part that differs from `before`.
    fn first_change(&self, before: &Prefix) -> Option<Reason> {
        let changed = self
            .0
            .iter()
            .zip(before.0)
            .position(|(now, then)| *now != then);
        changed.map(|at| ORDER[at])
    }
}

/// A miss, with the cached tokens before it and at it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Miss {
    pub reason: Reason,
    pub before: u64,
    pub cached: u64,
}

/// A request that was answered, as the next one is compared with.
#[derive(Clone, Copy)]
struct Answered {
    prefix: Prefix,
    /// When the answer came, in seconds since 1970: the cache is kept alive
    /// from its last use.
    at: u64,
    cached: u64,
}

/// What one session's requests left to compare with.
#[derive(Default)]
struct Session {
    /// The request out now, and when it was sent.
    pending: Option<(Prefix, u64)>,
    last: Option<Answered>,
}

impl Session {
    fn sent(&mut self, prefix: Prefix, now: u64) {
        self.pending = Some((prefix, now));
    }

    /// Take the answer to the pending request, which served `cached` tokens
    /// from the cache, and say why it missed if it did.
    fn answered(&mut self, cached: u64, now: u64) -> Option<Miss> {
        let (prefix, sent) = self.pending.take()?;
        let this = Answered {
            prefix,
            at: now,
            cached,
        };
        let before = self.last.replace(this)?;
        // Far fewer means under half: a cache that holds serves at least the
        // prefix it served last time, and a growing conversation only adds.
        if before.cached < LEAST || cached.saturating_mul(2) >= before.cached {
            return None;
        }
        let idle = sent.saturating_sub(before.at) > LIFETIME;
        let reason = prefix.first_change(&before.prefix).unwrap_or(if idle {
            Reason::Expired
        } else {
            Reason::Unknown
        });
        Some(Miss {
            reason,
            before: before.cached,
            cached,
        })
    }
}

fn sessions() -> &'static Mutex<HashMap<u64, Session>> {
    static SESSIONS: OnceLock<Mutex<HashMap<u64, Session>>> = OnceLock::new();
    SESSIONS.get_or_init(Default::default)
}

/// Note the prefix of a request just built for `conversation`. A call made
/// outside any session, with id 0, has nothing to compare with and is left
/// out.
pub(crate) fn sent(conversation: u64, prefix: Prefix) {
    if conversation == 0 {
        return;
    }
    if let Ok(mut sessions) = sessions().lock() {
        let now = crate::chat::now();
        sessions.entry(conversation).or_default().sent(prefix, now);
    }
}

/// The miss, if the answer to `conversation`'s pending request served far
/// fewer than `cached` tokens from the cache than the answer before it.
pub(crate) fn answered(conversation: u64, cached: u64) -> Option<Miss> {
    let mut sessions = sessions().lock().ok()?;
    let session = sessions.get_mut(&conversation)?;
    session.answered(cached, crate::chat::now())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A prefix whose every part hashes `seed`, but `part`, which hashes `with`.
    fn prefix_with(part: Option<Reason>, with: &str) -> Prefix {
        let mut prefix = Prefix::default();
        for p in ORDER {
            if Some(p) == part {
                prefix.set(p, with);
            } else {
                prefix.set(p, "seed");
            }
        }
        prefix
    }

    /// The reason the second of two requests `apart` seconds apart missed,
    /// the first serving 8,000 cached tokens and the second `cached`.
    fn second(first: Prefix, next: Prefix, apart: u64, cached: u64) -> Option<Reason> {
        let mut session = Session::default();
        session.sent(first, 1_000);
        assert_eq!(session.answered(8_000, 1_010), None);
        session.sent(next, 1_010 + apart);
        session
            .answered(cached, 1_020 + apart)
            .map(|miss| miss.reason)
    }

    #[test]
    fn each_changed_part_is_named_as_the_reason() {
        let same = prefix_with(None, "");
        for part in ORDER {
            let changed = prefix_with(Some(part), "changed");
            assert_eq!(second(same, changed, 30, 0), Some(part), "{part:?}");
        }
    }

    #[test]
    fn the_first_changed_part_wins_over_later_ones() {
        let mut changed = prefix_with(Some(Reason::Summary), "new summary");
        changed.set(Reason::Tools, "other tools");
        assert_eq!(
            second(prefix_with(None, ""), changed, 30, 0),
            Some(Reason::Summary)
        );
    }

    #[test]
    fn nothing_changed_after_a_long_pause_is_the_cache_expiring() {
        let same = prefix_with(None, "");
        assert_eq!(second(same, same, LIFETIME + 60, 0), Some(Reason::Expired));
        // A short pause with nothing changed is the provider's own doing.
        assert_eq!(second(same, same, 60, 0), Some(Reason::Unknown));
    }

    #[test]
    fn a_cache_that_holds_or_barely_dips_is_no_miss() {
        let same = prefix_with(None, "");
        let changed = prefix_with(Some(Reason::Memory), "changed");
        assert_eq!(second(same, same, 30, 9_000), None);
        assert_eq!(second(same, changed, 30, 4_000), None);
    }

    #[test]
    fn a_small_or_unreported_cache_never_misses() {
        let same = prefix_with(None, "");
        let mut session = Session::default();
        session.sent(same, 0);
        assert_eq!(session.answered(500, 1), None);
        session.sent(same, 2);
        assert_eq!(session.answered(0, 3), None);
        // An answer with no request noted for it is nothing to compare.
        assert_eq!(session.answered(0, 4), None);
    }

    #[test]
    fn a_miss_says_what_was_cached_before_and_after() {
        let mut session = Session::default();
        let same = prefix_with(None, "");
        session.sent(same, 0);
        session.answered(6_000, 1);
        session.sent(same, 2);
        let miss = session.answered(100, 3);
        assert_eq!(
            miss,
            Some(Miss {
                reason: Reason::Unknown,
                before: 6_000,
                cached: 100,
            })
        );
    }

    #[test]
    fn a_saved_reason_this_version_does_not_know_reads_as_unknown() {
        let reason: Reason = serde_json::from_str("\"weather\"").unwrap();
        assert_eq!(reason, Reason::Unknown);
        assert_eq!(
            serde_json::to_string(&Reason::Skills).unwrap(),
            "\"skills\""
        );
    }
}
