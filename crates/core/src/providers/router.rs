//! Where each model request goes, in turn, and how much of each account's
//! limits is used.
//!
//! The session's model is asked on its own account first. When that account
//! is rate limited past what the retries wait out, out of quota or credit,
//! or failing in another way `fallback::mends` covers, the same model is
//! asked on the next account set up, by tier -- a plan already paid for,
//! then an API key, then Letronna credit, then a free one, or in the order
//! `Settings.provider_order` gives -- and after those the models
//! `Settings.fallbacks` lists. An account is only asked once it lists the
//! model. `fallback::run` does the asking: each switch is told to the
//! frontends as one line, and an account that failed is passed over for
//! `fallback::COOLDOWN`.

use super::Tier;
use crate::chat::Connection;
use crate::failure::Failure;
use crate::fallback::{self, Fallback, Switch};
use crate::store::Settings;
use reqwest::header::HeaderMap;
use std::collections::{BTreeMap, HashMap};
use std::sync::{Mutex, PoisonError};

/// The tiers in the order Settings gives, then any it leaves out in their
/// usual order.
pub fn order(settings: &Settings) -> Vec<Tier> {
    let mut order = Vec::new();
    let named = settings
        .provider_order
        .iter()
        .filter_map(|n| Tier::parse(n));
    for tier in named.chain(Tier::ALL) {
        if !order.contains(&tier) {
            order.push(tier);
        }
    }
    order
}

/// Reach for the tiers in `order` from now on.
pub fn reorder(settings: &mut Settings, order: &[Tier]) {
    settings.provider_order = order.iter().map(|t| t.name().to_owned()).collect();
}

/// Send to `primary`, then on to the accounts and fallbacks after it, as
/// `fallback::run` does. `given` stands in for the route worked out here.
pub(crate) fn run(
    primary: &Connection,
    given: Option<Vec<Connection>>,
    send: impl FnMut(&Connection) -> Result<(), Failure>,
    switched: impl FnMut(Switch) -> Result<(), Failure>,
) -> Result<(), Failure> {
    let (chain, accounts) = match given {
        Some(chain) => (chain, Vec::new()),
        None => route(primary),
    };
    let usable = |c: &Connection| !accounts.iter().any(|a| fallback::same(a, c)) || serves(c);
    fallback::run(&chain, usable, send, switched)
}

/// Every place to ask, in turn, and those of them that are accounts the
/// router added, whose models are checked before one is asked. Settings are
/// read on each call, so a change made in the window counts in the terminal
/// from its next request.
fn route(primary: &Connection) -> (Vec<Connection>, Vec<Connection>) {
    let settings = crate::store::current_settings();
    let keys = crate::store::load_keys();
    let accounts = accounts(primary, &order(&settings), &ready(&keys, &settings));
    // Entries that do not read are passed over here; the frontends say why
    // when they are written.
    let fallbacks: Vec<_> = (settings.fallbacks.iter())
        .filter_map(|text| Fallback::parse(text).ok())
        .collect();
    let listed = fallback::chain(primary, &fallbacks, &keys, &settings.custom_endpoint);
    let mut chain = vec![primary.clone()];
    for next in accounts.iter().chain(&listed[1..]) {
        if !chain.iter().any(|c| fallback::same(c, next)) {
            chain.push(next.clone());
        }
    }
    (chain, accounts)
}

#[cfg(not(test))]
fn ready(
    keys: &HashMap<String, String>,
    settings: &Settings,
) -> Vec<(&'static str, String, String)> {
    crate::gateway::ready(keys, &settings.custom_endpoint)
}

/// No test reaches this machine's accounts.
#[cfg(test)]
fn ready(_: &HashMap<String, String>, _: &Settings) -> Vec<(&'static str, String, String)> {
    Vec::new()
}

/// The session's model on each of `ready` (name, base, key) but its own
/// account, at its tier or after it in `order`, in that order; within a tier
/// in the order the providers are offered.
fn accounts(
    primary: &Connection,
    order: &[Tier],
    ready: &[(&'static str, String, String)],
) -> Vec<Connection> {
    let rank = |name: &str| {
        let tier = super::find(name)?.info().tier;
        order.iter().position(|t| *t == tier)
    };
    let own = crate::gateway::named(&primary.endpoint)
        .and_then(rank)
        .unwrap_or(0);
    let mut accounts: Vec<_> = (ready.iter())
        .filter_map(|(name, base, key)| Some((rank(name)?, base, key)))
        .filter(|(rank, ..)| *rank >= own)
        .map(|(rank, base, key)| {
            let connection = Connection {
                endpoint: base.clone(),
                key: key.clone(),
                ..primary.clone()
            };
            (rank, connection)
        })
        .filter(|(_, c)| !fallback::same(c, primary))
        .collect();
    accounts.sort_by_key(|(rank, _)| *rank);
    accounts.into_iter().map(|(_, c)| c).collect()
}

/// Whether `connection`'s account lists its model.
fn serves(connection: &Connection) -> bool {
    // ponytail: each account's list is asked once a process; a model added
    // later is reached after a restart.
    static SERVED: Mutex<BTreeMap<String, Vec<String>>> = Mutex::new(BTreeMap::new());
    let endpoint = connection.endpoint.trim().trim_end_matches('/').to_owned();
    let kept = (SERVED.lock().unwrap_or_else(PoisonError::into_inner))
        .get(&endpoint)
        .cloned();
    let models = match kept {
        Some(models) => models,
        None => {
            let listed = match crate::gateway::named(&endpoint).and_then(super::find) {
                Some(provider) => provider.models(&endpoint, &connection.key),
                None => crate::chat::list_models(&endpoint, &connection.key),
            };
            // One that does not answer is asked again next time.
            let Ok(models) = listed else {
                return false;
            };
            (SERVED.lock().unwrap_or_else(PoisonError::into_inner))
                .insert(endpoint, models.clone());
            models
        }
    };
    models.iter().any(|m| m == connection.model.trim())
}

/// How much of an account's limits is used, as its last reply said.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Quota {
    /// The plan signed in with, for a subscription that says.
    pub plan: Option<String>,
    /// How much of the tightest limit is used, 0 to 100.
    pub used_percent: f64,
    /// When that limit starts again, in Unix seconds, if it was said.
    pub resets_at: Option<u64>,
}

/// The last quota each provider's replies told of, in memory only.
static QUOTAS: Mutex<BTreeMap<&'static str, Quota>> = Mutex::new(BTreeMap::new());

/// Keep the quota a reply from `endpoint` tells of.
pub(crate) fn heard(endpoint: &str, headers: &HeaderMap) {
    let Some(name) = crate::gateway::named(endpoint) else {
        return;
    };
    if let Some(quota) = super::find(name).and_then(|p| p.quota(headers)) {
        (QUOTAS.lock().unwrap_or_else(PoisonError::into_inner)).insert(name, quota);
    }
}

/// The last quota provider `name` told of since Letronna started.
pub fn quota(name: &str) -> Option<Quota> {
    (QUOTAS.lock().unwrap_or_else(PoisonError::into_inner))
        .get(name)
        .cloned()
}

/// The tightest of the standard rate-limit headers at `now`: OpenAI's
/// `x-ratelimit-*` and Anthropic's `anthropic-ratelimit-*`, for requests
/// and for tokens.
pub fn rate_limits(headers: &HeaderMap, now: u64) -> Option<Quota> {
    let get = |name: &str| headers.get(name)?.to_str().ok().map(str::trim);
    let number = |name: &str| get(name)?.parse::<f64>().ok();
    let mut tightest: Option<Quota> = None;
    for (limit, remaining, reset) in [
        (
            "x-ratelimit-limit-requests",
            "x-ratelimit-remaining-requests",
            "x-ratelimit-reset-requests",
        ),
        (
            "x-ratelimit-limit-tokens",
            "x-ratelimit-remaining-tokens",
            "x-ratelimit-reset-tokens",
        ),
        (
            "anthropic-ratelimit-requests-limit",
            "anthropic-ratelimit-requests-remaining",
            "anthropic-ratelimit-requests-reset",
        ),
        (
            "anthropic-ratelimit-tokens-limit",
            "anthropic-ratelimit-tokens-remaining",
            "anthropic-ratelimit-tokens-reset",
        ),
    ] {
        let (Some(limit), Some(remaining)) = (number(limit), number(remaining)) else {
            continue;
        };
        if limit <= 0.0 {
            continue;
        }
        let used_percent = ((limit - remaining) / limit * 100.0).clamp(0.0, 100.0);
        if tightest
            .as_ref()
            .is_none_or(|t| used_percent > t.used_percent)
        {
            tightest = Some(Quota {
                plan: None,
                used_percent,
                resets_at: get(reset).and_then(|r| reset_at(r, now)),
            });
        }
    }
    tightest
}

/// When a limit starts again, given as a date (Anthropic's), seconds, or a
/// wait such as `6m0s` or `20ms` (OpenAI's).
fn reset_at(value: &str, now: u64) -> Option<u64> {
    if let Ok(date) = chrono::DateTime::parse_from_rfc3339(value) {
        return u64::try_from(date.timestamp()).ok();
    }
    if let Ok(seconds) = value.parse::<f64>() {
        return Some(now + seconds.max(0.0).ceil() as u64);
    }
    let (mut seconds, mut rest) = (0.0, value);
    while !rest.is_empty() {
        let digits = rest.find(|c: char| !(c.is_ascii_digit() || c == '.'))?;
        let n: f64 = rest[..digits].parse().ok()?;
        rest = &rest[digits..];
        let unit = rest
            .find(|c: char| c.is_ascii_digit())
            .unwrap_or(rest.len());
        seconds += n * match &rest[..unit] {
            "h" => 3600.0,
            "m" => 60.0,
            "s" => 1.0,
            "ms" => 0.001,
            _ => return None,
        };
        rest = &rest[unit..];
    }
    Some(now + seconds.ceil() as u64)
}

#[cfg(test)]
mod tests {
    use super::*;
    use reqwest::header::{HeaderName, HeaderValue};

    fn headers(pairs: &[(&'static str, &str)]) -> HeaderMap {
        (pairs.iter())
            .map(|(k, v)| {
                (
                    HeaderName::from_static(k),
                    HeaderValue::from_str(v).unwrap(),
                )
            })
            .collect()
    }

    #[test]
    fn the_tightest_standard_limit_is_the_quota() {
        let openai = headers(&[
            ("x-ratelimit-limit-requests", "500"),
            ("x-ratelimit-remaining-requests", "499"),
            ("x-ratelimit-reset-requests", "120ms"),
            ("x-ratelimit-limit-tokens", "1000"),
            ("x-ratelimit-remaining-tokens", "250"),
            ("x-ratelimit-reset-tokens", "6m0.5s"),
        ]);
        let quota = rate_limits(&openai, 1_000).unwrap();
        assert_eq!(quota.used_percent, 75.0);
        assert_eq!(quota.resets_at, Some(1_361));

        let anthropic = headers(&[
            ("anthropic-ratelimit-requests-limit", "50"),
            ("anthropic-ratelimit-requests-remaining", "0"),
            ("anthropic-ratelimit-requests-reset", "2026-10-11T12:00:00Z"),
        ]);
        let quota = rate_limits(&anthropic, 0).unwrap();
        assert_eq!(quota.used_percent, 100.0);
        assert_eq!(quota.resets_at, Some(1_791_720_000));

        assert_eq!(rate_limits(&HeaderMap::new(), 0), None);
        assert_eq!(reset_at("1h2m3s", 0), Some(3_723));
        assert_eq!(reset_at("30", 10), Some(40));
        assert_eq!(reset_at("soon", 0), None);
    }

    #[test]
    fn tiers_follow_the_setting_then_their_usual_order() {
        let mut settings = Settings::default();
        assert_eq!(order(&settings), Tier::ALL);
        settings.provider_order = vec![
            "free".into(),
            "nonsense".into(),
            "credit".into(),
            "free".into(),
        ];
        assert_eq!(
            order(&settings),
            [Tier::Free, Tier::Credit, Tier::Subscription, Tier::ApiKey]
        );
        reorder(&mut settings, &[Tier::ApiKey]);
        assert_eq!(settings.provider_order, ["api_key"]);
    }

    #[test]
    fn accounts_come_in_tier_order_from_the_sessions_own() {
        let ready = |names: &[&'static str]| -> Vec<_> {
            (names.iter())
                .map(|n| (*n, crate::gateway::base_url(n, ""), format!("key-{n}")))
                .collect()
        };
        let all = ready(&["Letronna", "OpenAI", "ChatGPT", "Ollama"]);
        let on = |name: &str| Connection {
            endpoint: crate::gateway::base_url(name, ""),
            model: "gpt-5".into(),
            ..Default::default()
        };
        let names = |order: &[Tier], from: &str| -> Vec<_> {
            (accounts(&on(from), order, &all).iter())
                .map(|c| crate::gateway::named(&c.endpoint).unwrap())
                .collect()
        };
        // From a plan, every other account, by tier.
        assert_eq!(
            names(&Tier::ALL, "ChatGPT"),
            ["OpenAI", "Letronna", "Ollama"]
        );
        // From a key, only that tier and those after it.
        assert_eq!(names(&Tier::ALL, "OpenAI"), ["Letronna", "Ollama"]);
        // The user's order counts.
        let free_first = [Tier::Free, Tier::Subscription, Tier::ApiKey, Tier::Credit];
        assert_eq!(
            names(&free_first, "Ollama"),
            ["ChatGPT", "OpenAI", "Letronna"]
        );
        // Each takes its own key, and the session's model.
        let first = &accounts(&on("ChatGPT"), &Tier::ALL, &all)[0];
        assert_eq!(
            (first.key.as_str(), first.model.as_str()),
            ("key-OpenAI", "gpt-5")
        );
    }
}
