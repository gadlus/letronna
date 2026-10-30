//! How much of each account's plan is used: its windows ("5-hour",
//! "Weekly"), how full each is and when each starts again.
//!
//! Two ways in. Each reply on a plan says what its limits stand at, in its
//! headers (`Provider::quota`), which `router::heard` passes on here. And a
//! plan that has a page for it is asked outright (`Provider::usage`) when the
//! Accounts page opens, `/limits` is typed, or a phone asks: ChatGPT's
//! `wham/usage`, Claude's `oauth/usage`, OpenCode Go's `usage`. Asking is
//! held to once every `AUTOMATIC` per account, or `ASKED` when the user asks
//! again, since some of those pages turn quick repeats away.
//!
//! What is learned is kept in `usage.json` in the data folder, which the
//! window, the terminal and a phone's questions all read, so each sees what
//! the others learned. It holds percentages and times, never a key.
//!
//! An account whose window is full is not asked again until that window
//! starts again (`spent`): the router goes on to the next account at once
//! rather than waiting for a refusal.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Mutex, PoisonError};
use std::time::SystemTime;

/// How long what was asked holds before opening a page asks again.
pub const AUTOMATIC: u64 = 5 * 60;
/// How long it holds against the user asking again outright.
pub const ASKED: u64 = 30;
/// The longest an account is passed over for a full window: a reset further
/// off than this is more likely a misread than a plan's.
const LONGEST_SPENT: u64 = 24 * 60 * 60;

/// One of a plan's limits.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Window {
    /// What it is called: "5-hour", "Weekly", "Weekly · Opus", "Monthly".
    pub label: String,
    /// How much of it is used, 0 to 100.
    pub used_percent: f64,
    /// When it starts again, in Unix seconds, when that was said.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resets_at: Option<u64>,
}

impl Window {
    pub fn left_percent(&self) -> f64 {
        (100.0 - self.used_percent).clamp(0.0, 100.0)
    }

    /// Whether it is full at `now`: used up, and not started again yet.
    pub fn full(&self, now: u64) -> bool {
        self.used_percent >= 100.0 && self.resets_at.is_none_or(|at| at > now)
    }
}

/// What an account's plan stands at.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Usage {
    /// The plan, as the provider names it ("plus", "max").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan: Option<String>,
    #[serde(default)]
    pub windows: Vec<Window>,
    /// Something to say in place of windows: a key with no plan on it, say.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    /// When it was learned, in Unix seconds.
    #[serde(default)]
    pub checked_at: u64,
    /// When the provider's own page was last asked, which the holds count
    /// from; a reply's headers do not count.
    #[serde(default)]
    pub asked_at: u64,
}

impl Usage {
    /// The fullest window, which says how close the account is to stopping.
    pub fn tightest(&self) -> Option<&Window> {
        (self.windows.iter()).max_by(|a, b| a.used_percent.total_cmp(&b.used_percent))
    }

    /// When the account can be asked again, while a window of it is full.
    pub fn spent(&self, now: u64) -> Option<u64> {
        let full = self.windows.iter().filter(|w| w.full(now));
        let back = full.map(|w| {
            w.resets_at
                .unwrap_or(now + crate::fallback::COOLDOWN.as_secs())
        });
        back.max().map(|at| at.min(now + LONGEST_SPENT))
    }
}

fn path() -> PathBuf {
    crate::chat::data_dir().join("usage.json")
}

/// The file as last read: where, when it was written, and what it held.
type Seen = (PathBuf, Option<(SystemTime, u64)>, BTreeMap<String, Usage>);

/// What each account's usage was last known to be, read again only once
/// the file has changed: the router asks before every request, and the
/// window and the terminal each write it.
fn known() -> &'static Mutex<Option<Seen>> {
    static KNOWN: Mutex<Option<Seen>> = Mutex::new(None);
    &KNOWN
}

fn read_file() -> BTreeMap<String, Usage> {
    crate::state::read(&path()).unwrap_or_default()
}

/// Everything known, from the file as it is now.
pub fn all() -> BTreeMap<String, Usage> {
    let path = path();
    let stamp = std::fs::metadata(&path)
        .ok()
        .and_then(|meta| Some((meta.modified().ok()?, meta.len())));
    let mut known = known().lock().unwrap_or_else(PoisonError::into_inner);
    if let Some((at, was, map)) = known.as_ref()
        && *at == path
        && *was == stamp
    {
        return map.clone();
    }
    let map = read_file();
    *known = Some((path, stamp, map.clone()));
    map
}

/// What is known of `account`'s usage, without asking anyone.
pub fn get(account: &str) -> Option<Usage> {
    all().remove(account)
}

/// Keep `usage` for `account`, in the file, which the next read takes up.
fn keep(account: &str, usage: Usage) {
    let _ = std::fs::create_dir_all(crate::chat::data_dir());
    let _ = crate::state::update::<BTreeMap<String, Usage>>(&path(), |disk| {
        if !disk.is_object() {
            *disk = Value::Object(Default::default());
        }
        if let (Some(map), Ok(value)) = (disk.as_object_mut(), serde_json::to_value(&usage)) {
            map.insert(account.to_owned(), value);
        }
    });
    // A write within the clock's step keeps the old time and length.
    if let Ok(mut known) = known().lock() {
        *known = None;
    }
}

/// Forget what is known of `account`, as when it is removed.
pub fn forget(account: &str) {
    let _ = crate::state::update::<BTreeMap<String, Usage>>(&path(), |disk| {
        if let Some(map) = disk.as_object_mut() {
            map.remove(account);
        }
    });
    if let Ok(mut known) = known().lock() {
        *known = None;
    }
}

/// Take what a reply's headers said of `account`'s limits. A reply that
/// names its windows replaces what was known of them; one that gives only
/// the tightest limit keeps the windows asked for, unless it says that limit
/// is now full, which the router must hear of at once.
pub(crate) fn heard(account: &str, quota: &super::Quota) {
    let now = crate::chat::now();
    let mut usage = get(account).unwrap_or_default();
    if !quota.windows.is_empty() {
        usage.windows = quota.windows.clone();
    } else if quota.used_percent >= 100.0 || usage.windows.is_empty() {
        usage.windows = vec![Window {
            label: "Rate limit".into(),
            used_percent: quota.used_percent,
            resets_at: quota.resets_at,
        }];
    } else {
        return;
    }
    if quota.plan.is_some() {
        usage.plan = quota.plan.clone();
    } else if usage.plan.is_none() {
        usage.plan = (super::auth::tokens(account).map(|t| t.plan)).filter(|p| !p.is_empty());
    }
    usage.checked_at = now;
    keep(account, usage);
}

/// When `account` can be asked again, while a window of its plan is full.
pub fn spent(account: &str) -> Option<u64> {
    if account.is_empty() {
        return None;
    }
    get(account)?.spent(crate::chat::now())
}

/// `account`'s usage, asked of its provider when what is known is older than
/// the hold (`ASKED` when `asked` by the user, else `AUTOMATIC`), and what is
/// known otherwise. An account whose provider has no page for it gives what
/// its replies said; one that cannot be asked says why.
pub fn check(account: &str, asked: bool) -> Result<Usage, String> {
    let known = get(account);
    let now = crate::chat::now();
    let hold = if asked { ASKED } else { AUTOMATIC };
    if let Some(usage) = known.as_ref().filter(|u| now < u.asked_at + hold) {
        return Ok(usage.clone());
    }
    let Some(provider) = super::find(account) else {
        return known.ok_or_else(|| format!("There is no account called {account}."));
    };
    let keys = crate::store::load_keys();
    let saved = keys
        .get(crate::gateway::key_slot(account))
        .cloned()
        .unwrap_or_default();
    let custom = crate::store::current_settings().custom_endpoint;
    let (base, key) = crate::gateway::resolve(account, &custom, &saved);
    if key.trim().is_empty() {
        return known.ok_or_else(|| format!("{account} is not set up."));
    }
    match provider.usage(account, &base, &key) {
        Ok(Some(mut usage)) => {
            usage.checked_at = now;
            usage.asked_at = now;
            if usage.plan.is_none() {
                usage.plan = known.as_ref().and_then(|u| u.plan.clone()).or_else(|| {
                    super::auth::tokens(account)
                        .map(|t| t.plan)
                        .filter(|p| !p.is_empty())
                });
            }
            keep(account, usage.clone());
            Ok(usage)
        }
        Ok(None) => Ok(known.unwrap_or_default()),
        // What was known stays: a page that is down says nothing new.
        Err(why) => match known {
            Some(mut usage) => {
                usage.asked_at = now;
                keep(account, usage.clone());
                Ok(usage)
            }
            None => Err(why),
        },
    }
}

/// `account`'s usage in a line: "Plus · 5-hour 62% left · resets in 3h 10m".
/// Empty when nothing is known.
pub fn line(usage: &Usage, now: u64) -> String {
    let mut parts: Vec<String> = usage.plan.iter().map(|p| capitalised(p)).collect();
    if let Some(note) = &usage.note {
        parts.push(note.clone());
    }
    for window in &usage.windows {
        parts.push(window_line(window, now));
    }
    parts.join(" · ")
}

/// A window in words: "5-hour 62% left, resets in 3h 10m", or "Weekly
/// limit reached, back at 14:20".
pub fn window_line(window: &Window, now: u64) -> String {
    if window.full(now) {
        return match window.resets_at {
            Some(at) => format!(
                "{} limit reached, back at {}",
                window.label,
                crate::failure::clock(at)
            ),
            None => format!("{} limit reached", window.label),
        };
    }
    let mut text = format!("{} {:.0}% left", window.label, window.left_percent());
    if let Some(at) = window.resets_at.filter(|&at| at > now) {
        text.push_str(&format!(", resets in {}", until(at - now)));
    }
    text
}

/// A wait in words: "45m", "3h 10m", "2d".
pub fn until(seconds: u64) -> String {
    let mins = seconds.div_ceil(60);
    match mins {
        0..60 => format!("{mins}m"),
        60..1440 => format!("{}h {}m", mins / 60, mins % 60),
        _ => format!("{}d", mins / 1440),
    }
}

pub fn capitalised(word: &str) -> String {
    let mut chars = word.chars();
    chars
        .next()
        .map(|c| c.to_uppercase().chain(chars).collect())
        .unwrap_or_default()
}

/// A window's name from its length in seconds: "5-hour", "Weekly".
pub(crate) fn length_label(seconds: u64) -> String {
    match seconds {
        604_800 => "Weekly".into(),
        86_400 => "Daily".into(),
        s if s >= 28 * 86_400 => "Monthly".into(),
        s if s % 86_400 == 0 => format!("{}-day", s / 86_400),
        s if s % 3_600 == 0 => format!("{}-hour", s / 3_600),
        s => format!("{}-minute", s.div_ceil(60)),
    }
}

/// A time as a page gives it: Unix seconds or milliseconds, or an RFC 3339
/// date.
pub(crate) fn instant(value: &Value) -> Option<u64> {
    if let Some(n) = value.as_f64() {
        let n = if n > 1e11 { n / 1000.0 } else { n };
        return (n > 0.0).then_some(n as u64);
    }
    let text = value.as_str()?.trim();
    if let Ok(n) = text.parse::<f64>() {
        return instant(&Value::from(n));
    }
    chrono::DateTime::parse_from_rfc3339(text)
        .ok()
        .and_then(|at| u64::try_from(at.timestamp()).ok())
}

/// A percentage as a page gives it.
pub(crate) fn percent(value: &Value) -> Option<f64> {
    value
        .as_f64()
        .or_else(|| value.as_str()?.trim().parse().ok())
        .filter(|p: &f64| p.is_finite())
        .map(|p| p.clamp(0.0, 100.0))
}

/// GET `url` with `request` signing it, as JSON: the reply, or why not.
pub(crate) fn ask(
    request: reqwest::blocking::RequestBuilder,
    who: &str,
) -> Result<Option<Value>, String> {
    let reply = request
        .timeout(std::time::Duration::from_secs(15))
        .send()
        .map_err(|_| format!("Cannot reach {who} to read its usage."))?;
    match reply.status().as_u16() {
        200..=299 => reply
            .json()
            .map(Some)
            .map_err(|_| format!("{who} sent usage Letronna cannot read.")),
        401 | 403 => Err(format!("{who} turned the sign-in down. Sign in again.")),
        404 => Ok(None),
        429 => Err(format!(
            "{who} asks to wait before its usage is read again."
        )),
        status => Err(format!("{who} could not give its usage (HTTP {status}).")),
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// Take `account`'s plan as full until `back`, as a reply would say.
    pub(crate) fn full(account: &str, back: u64) {
        heard(
            account,
            &super::super::Quota {
                plan: None,
                used_percent: 100.0,
                resets_at: Some(back),
                windows: Vec::new(),
            },
        );
    }

    #[test]
    fn a_full_window_holds_the_account_back_until_it_starts_again() {
        let usage = Usage {
            windows: vec![
                Window {
                    label: "5-hour".into(),
                    used_percent: 100.0,
                    resets_at: Some(2_000),
                },
                Window {
                    label: "Weekly".into(),
                    used_percent: 40.0,
                    resets_at: Some(9_000),
                },
            ],
            ..Default::default()
        };
        assert_eq!(usage.spent(1_000), Some(2_000));
        assert_eq!(usage.spent(2_000), None);
        assert_eq!(usage.tightest().unwrap().label, "5-hour");
        assert_eq!(
            window_line(&usage.windows[1], 1_000),
            "Weekly 60% left, resets in 2h 14m"
        );
        assert!(
            window_line(&usage.windows[0], 1_000).starts_with("5-hour limit reached, back at ")
        );
    }

    #[test]
    fn what_a_reply_says_is_kept_for_the_other_processes_and_counts_at_once() {
        let _held = super::super::auth::tests::vault("usage-heard");
        assert_eq!(spent("ChatGPT"), None);
        let now = crate::chat::now();
        heard(
            "ChatGPT",
            &super::super::Quota {
                plan: Some("plus".into()),
                used_percent: 100.0,
                resets_at: Some(now + 600),
                windows: Vec::new(),
            },
        );
        assert_eq!(spent("ChatGPT"), Some(now + 600));
        // Read back from the file, as another process would.
        let kept = read_file();
        assert_eq!(kept["ChatGPT"].plan.as_deref(), Some("plus"));
        forget("ChatGPT");
        assert_eq!(spent("ChatGPT"), None);
    }

    #[test]
    fn times_and_lengths_read_as_pages_give_them() {
        assert_eq!(
            instant(&serde_json::json!(1_793_100_000)),
            Some(1_793_100_000)
        );
        assert_eq!(
            instant(&serde_json::json!(1_793_100_000_000u64)),
            Some(1_793_100_000)
        );
        assert_eq!(
            instant(&serde_json::json!("2026-10-27T12:00:00.000+00:00")),
            Some(1_793_102_400)
        );
        assert_eq!(length_label(18_000), "5-hour");
        assert_eq!(length_label(604_800), "Weekly");
        assert_eq!(length_label(2_592_000), "Monthly");
        assert_eq!(percent(&serde_json::json!("12.5")), Some(12.5));
        assert_eq!(until(10_800 + 600), "3h 10m");
    }
}
