//! Where each model request goes, in turn, and how much of each account's
//! limits is used.
//!
//! The session's model is asked on its own account first. When that account
//! is rate limited past what the retries wait out, out of quota or credit,
//! or failing in another way `fallback::mends` covers, the same model is
//! asked on the other accounts set up, by tier -- a plan already paid for,
//! then an API key, then Letronna credit, then a free one, or in the order
//! `Settings.provider_order` gives. Those at the session's own tier or after
//! it come first; those before it come after them, rather than not at all,
//! so a Letronna account out of credit still reaches a key of the user's
//! own. After those come the models `Settings.fallbacks` lists, and last the
//! default model, when it is on another gateway. An account is only asked
//! once it lists the model. `fallback::run` does the asking: each switch is
//! told to the frontends as one line, and an account that failed is passed
//! over for `fallback::COOLDOWN`.

use super::Tier;
use crate::chat::Connection;
use crate::failure::Failure;
use crate::fallback::{self, Fallback, Switch};
use crate::store::Settings;
use reqwest::header::HeaderMap;
use std::collections::{BTreeMap, HashMap};
use std::hash::{Hash, Hasher};
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};

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
/// Settings are read on each call, so a change made in the window counts in
/// the terminal from its next request.
pub(crate) fn run(
    primary: &Connection,
    given: Option<Vec<Connection>>,
    send: impl FnMut(&Connection) -> Result<(), Failure>,
    switched: impl FnMut(Switch) -> Result<(), Failure>,
) -> Result<(), Failure> {
    let (chain, accounts, custom) = match given {
        Some(chain) => (chain, Vec::new(), String::new()),
        None => {
            let settings = crate::store::current_settings();
            let keys = crate::store::load_keys();
            let ready = ready(&keys, &settings);
            let (mut chain, accounts) = route(primary, &settings, &keys, &ready);
            // Last of all, a model on this computer, asked only once the
            // others could not be reached (`fallback::run`).
            if let Some(local) = stand_in(primary, &settings)
                && !chain.iter().any(|c| fallback::same(c, &local))
            {
                chain.push(local);
            }
            (chain, accounts, settings.custom_endpoint)
        }
    };
    let usable = |c: &Connection| !accounts.iter().any(|a| fallback::same(a, c)) || serves(c);
    fallback::run(&chain, &custom, usable, send, switched)
}

/// Every place to ask, in turn, and those of them that are accounts the
/// router added, whose models are checked before one is asked. `ready` is
/// the gateways set up, as `gateway::ready` gives them.
fn route(
    primary: &Connection,
    settings: &Settings,
    keys: &HashMap<String, String>,
    ready: &[(&'static str, String, String)],
) -> (Vec<Connection>, Vec<Connection>) {
    // What is said on this computer stays there: no other account, no
    // fallback, no default model elsewhere.
    if crate::on_device::at(&primary.endpoint) {
        return (vec![primary.clone()], Vec::new());
    }
    let accounts = accounts(primary, &order(settings), ready);
    // Entries that do not read are passed over here; the frontends say why
    // when they are written.
    let mut fallbacks: Vec<_> = (settings.fallbacks.iter())
        .filter_map(|text| Fallback::parse(text).ok())
        .collect();
    fallbacks.extend(default_elsewhere(primary, settings, ready));
    let listed = fallback::chain(primary, &fallbacks, keys, &settings.custom_endpoint);
    let mut chain = vec![primary.clone()];
    for next in accounts.iter().chain(&listed[1..]) {
        if !chain.iter().any(|c| fallback::same(c, next)) {
            chain.push(next.clone());
        }
    }
    (chain, accounts)
}

/// The default model, when it is on a gateway set up other than the
/// session's. An account is only asked for the session's own model id,
/// which a key of the user's own seldom lists by a hosted model's name, so
/// without it a session left on Letronna would end at the account's
/// refusal while the model they chose for new sessions waits on their key.
fn default_elsewhere(
    primary: &Connection,
    settings: &Settings,
    ready: &[(&'static str, String, String)],
) -> Option<Fallback> {
    let address = |text: &str| text.trim().trim_end_matches('/').to_owned();
    let (gateway, model) = (settings.gateway.trim(), settings.model.trim());
    let (_, base, _) = ready.iter().find(|(name, ..)| *name == gateway)?;
    (!model.is_empty() && address(base) != address(&primary.endpoint)).then(|| Fallback {
        gateway: gateway.to_owned(),
        model: model.to_owned(),
    })
}

#[cfg(not(test))]
fn stand_in(primary: &Connection, settings: &Settings) -> Option<Connection> {
    crate::on_device::reach::stand_in(primary, settings)
}

/// No test reaches this machine's downloads.
#[cfg(test)]
fn stand_in(_: &Connection, _: &Settings) -> Option<Connection> {
    None
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
/// account, by tier in `order`: those at its tier or after it first, then
/// those before it; within a tier in the order the providers are offered. A
/// gateway no provider declares, as the user's own and those plugins add
/// are, is ranked as Custom is, among the API keys.
fn accounts(
    primary: &Connection,
    order: &[Tier],
    ready: &[(&'static str, String, String)],
) -> Vec<Connection> {
    let rank = |name: Option<&str>| {
        let tier = (name.and_then(super::find)).map_or(Tier::ApiKey, |p| p.info().tier);
        order.iter().position(|t| *t == tier).unwrap_or(order.len())
    };
    let own = rank(crate::gateway::named(&primary.endpoint));
    let mut accounts: Vec<_> = (ready.iter())
        // A model here is no account for another's: it never serves its id.
        .filter(|(name, ..)| *name != crate::on_device::NAME)
        .map(|(name, base, key)| {
            let connection = Connection {
                endpoint: base.clone(),
                key: key.clone(),
                ..primary.clone()
            };
            let rank = rank(Some(name));
            ((rank < own, rank), connection)
        })
        .filter(|(_, c)| !fallback::same(c, primary))
        .collect();
    accounts.sort_by_key(|(rank, _)| *rank);
    accounts.into_iter().map(|(_, c)| c).collect()
}

/// How long an account's list of models is taken as it was: a model the
/// server gains later is reached within this, and a new key or address at
/// once, as they are listed apart.
const SERVED_FOR: Duration = Duration::from_secs(5 * 60);

/// Each account's models as last listed, by its address and a hash of its
/// key -- the key itself is not kept twice -- with when.
type Served = BTreeMap<(String, u64), (Instant, Vec<String>)>;
static SERVED: Mutex<Served> = Mutex::new(BTreeMap::new());

fn served_as(endpoint: &str, key: &str) -> (String, u64) {
    let mut hash = std::hash::DefaultHasher::new();
    key.trim().hash(&mut hash);
    (
        endpoint.trim().trim_end_matches('/').to_owned(),
        hash.finish(),
    )
}

/// Take `models` as what the account at `endpoint` with `key` lists, as a
/// model list just asked of it said, so the router and the model lists
/// agree without asking twice.
pub(crate) fn listed(endpoint: &str, key: &str, models: &[String]) {
    (SERVED.lock().unwrap_or_else(PoisonError::into_inner))
        .insert(served_as(endpoint, key), (Instant::now(), models.to_vec()));
}

/// Ask every account for its models again before it is next tried: a
/// gateway was added, changed or removed, or a key was saved.
pub fn forget() {
    (SERVED.lock().unwrap_or_else(PoisonError::into_inner)).clear();
}

/// Whether `connection`'s account lists its model.
fn serves(connection: &Connection) -> bool {
    let at = served_as(&connection.endpoint, &connection.key);
    let kept = (SERVED.lock().unwrap_or_else(PoisonError::into_inner))
        .get(&at)
        .filter(|(when, _)| when.elapsed() < SERVED_FOR)
        .map(|(_, models)| models.clone());
    let models = match kept {
        Some(models) => models,
        None => {
            let endpoint = &at.0;
            let listed = match crate::gateway::named(endpoint).and_then(super::find) {
                Some(provider) => provider.models(endpoint, &connection.key),
                None => crate::chat::list_models(endpoint, &connection.key),
            };
            // One that does not answer is asked again next time.
            let Ok(models) = listed else {
                return false;
            };
            self::listed(endpoint, &connection.key, &models);
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

/// An account's plan and usage in a line, for the Accounts page and
/// `/connect`: what its last reply said, or the plan it signed in with.
/// Empty when nothing is known yet.
pub fn usage(name: &str) -> String {
    if name == crate::gateway::LETRONNA {
        return crate::auth::account()
            .map(|a| match a.plan_name() {
                "" => format!("{} credit", a.credit()),
                plan => format!("{plan} · {} credit", a.credit()),
            })
            .unwrap_or_default();
    }
    let signed = super::auth::tokens(name)
        .map(|t| t.plan)
        .filter(|p| !p.is_empty());
    describe(quota(name), signed, crate::chat::now())
}

fn describe(quota: Option<Quota>, signed: Option<String>, now: u64) -> String {
    let plan = quota.as_ref().and_then(|q| q.plan.clone()).or(signed);
    let mut parts: Vec<String> = plan.into_iter().map(|p| capitalised(&p)).collect();
    if let Some(q) = quota {
        parts.push(format!("{:.0}% used", q.used_percent));
        if let Some(at) = q.resets_at.filter(|&at| at > now) {
            let mins = (at - now).div_ceil(60);
            parts.push(match mins {
                0..60 => format!("resets in {mins}m"),
                60..1440 => format!("resets in {}h {}m", mins / 60, mins % 60),
                _ => format!("resets in {}d", mins / 1440),
            });
        }
    }
    parts.join(" · ")
}

fn capitalised(word: &str) -> String {
    let mut chars = word.chars();
    chars
        .next()
        .map(|c| c.to_uppercase().chain(chars).collect())
        .unwrap_or_default()
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

    #[test]
    fn a_new_key_a_stale_list_or_a_forget_asks_the_account_again() {
        // Nothing answers on the discard port, so asking it fails at once.
        let at = "http://127.0.0.1:9/served/v1";
        let on = |key: &str| Connection {
            endpoint: format!("{at}/"),
            key: key.into(),
            model: "m".into(),
            ..Default::default()
        };
        listed(at, "old", &["m".into()]);
        assert!(serves(&on("old")));
        // Another key is another account, whose list is asked for afresh.
        assert!(!serves(&on("new")));
        forget();
        assert!(!serves(&on("old")));
        // A list kept past its time is asked for again too.
        if let Some(then) = Instant::now().checked_sub(SERVED_FOR + Duration::from_secs(1)) {
            (SERVED.lock().unwrap_or_else(PoisonError::into_inner))
                .insert(served_as(at, "old"), (then, vec!["m".into()]));
            assert!(!serves(&on("old")));
        }
    }

    #[test]
    fn usage_reads_as_plan_then_use_then_reset() {
        let q = |plan: Option<&str>, at| Quota {
            plan: plan.map(str::to_owned),
            used_percent: 42.4,
            resets_at: at,
        };
        assert_eq!(describe(None, None, 0), "");
        assert_eq!(describe(None, Some("plus".into()), 0), "Plus");
        assert_eq!(
            describe(Some(q(Some("pro"), Some(3_900))), Some("plus".into()), 0),
            "Pro · 42% used · resets in 1h 5m"
        );
        assert_eq!(
            describe(Some(q(None, Some(10))), None, 0),
            "42% used · resets in 1m"
        );
        assert_eq!(describe(Some(q(None, Some(5))), None, 9), "42% used");
    }

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
        // From a key, that tier and those after it, then those before it.
        assert_eq!(
            names(&Tier::ALL, "OpenAI"),
            ["Letronna", "Ollama", "ChatGPT"]
        );
        // From Letronna credit, a key comes after a free one rather than never.
        assert_eq!(
            names(&Tier::ALL, "Letronna"),
            ["Ollama", "ChatGPT", "OpenAI"]
        );
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

    /// A gateway set up, as `gateway::ready` gives it.
    fn set_up(name: &'static str, base: &str) -> (&'static str, String, String) {
        (name, base.to_owned(), format!("key-{name}"))
    }

    fn letronna(model: &str) -> Connection {
        Connection {
            endpoint: crate::gateway::base_url(crate::gateway::LETRONNA, ""),
            model: model.into(),
            ..Default::default()
        }
    }

    #[test]
    fn gateways_no_provider_declares_are_reached_as_keys_are() {
        let ready = [
            set_up("Letronna", &crate::auth::hosted_base()),
            set_up("Ollama", &crate::gateway::base_url("Ollama", "")),
            set_up("Office", "https://office.router.test/v1"),
            set_up("Custom", "https://custom.router.test/v1"),
        ];
        let endpoints: Vec<_> = (accounts(&letronna("gpt-5"), &Tier::ALL, &ready).iter())
            .map(|c| c.endpoint.clone())
            .collect();
        assert_eq!(
            endpoints,
            [
                crate::gateway::base_url("Ollama", ""),
                "https://office.router.test/v1".into(),
                "https://custom.router.test/v1".into(),
            ]
        );
    }

    #[test]
    fn the_default_model_on_another_gateway_is_asked_last() {
        let custom = "https://default.router.test/v1";
        let ready = [
            set_up("Letronna", &crate::auth::hosted_base()),
            set_up("Custom", custom),
        ];
        let settings = Settings {
            gateway: "Custom".into(),
            model: "qwen3".into(),
            custom_endpoint: custom.into(),
            ..Default::default()
        };
        let asked = |primary: &Connection, settings: &Settings| -> Vec<_> {
            (route(primary, settings, &HashMap::new(), &ready).0.iter())
                .map(|c| (c.endpoint.clone(), c.model.clone()))
                .collect()
        };
        let hosted = letronna("letronna/auto").endpoint;
        assert_eq!(
            asked(&letronna("letronna/auto"), &settings),
            [
                (hosted.clone(), "letronna/auto".into()),
                // The account, should it list the session's model.
                (custom.into(), "letronna/auto".into()),
                (custom.into(), "qwen3".into()),
            ]
        );
        // On its own gateway it is the session's to choose, not a fallback.
        let on_custom = Connection {
            endpoint: custom.into(),
            ..letronna("other")
        };
        assert_eq!(
            asked(&on_custom, &settings),
            [(custom.into(), "other".into()), (hosted, "other".into())]
        );
        // Nor is it asked on a gateway not set up.
        let elsewhere = Settings {
            gateway: "OpenRouter".into(),
            ..settings
        };
        assert_eq!(asked(&letronna("letronna/auto"), &elsewhere).len(), 2);
    }

    #[test]
    fn a_chat_on_this_computer_is_never_handed_to_another_gateway() {
        let primary = Connection {
            endpoint: crate::on_device::BASE.into(),
            model: "bonsai-2-4b".into(),
            ..Default::default()
        };
        let settings = Settings {
            fallbacks: vec!["OpenRouter:some/model".into()],
            gateway: "Ollama".into(),
            model: "qwen3".into(),
            ..Default::default()
        };
        let ready = [
            (
                "Letronna",
                "https://api.letronna.ai/v1".to_owned(),
                "ltr".to_owned(),
            ),
            (
                "Ollama",
                "http://localhost:11434/v1".to_owned(),
                String::new(),
            ),
            (
                crate::on_device::NAME,
                crate::on_device::BASE.to_owned(),
                String::new(),
            ),
        ];
        let (chain, accounts) = route(&primary, &settings, &HashMap::new(), &ready);
        assert_eq!(chain.len(), 1);
        assert!(fallback::same(&chain[0], &primary));
        assert!(accounts.is_empty());
    }

    #[test]
    fn this_computer_is_never_an_account_for_another_model() {
        let primary = Connection {
            endpoint: "https://api.letronna.ai/v1".into(),
            model: "bonsai-2-4b".into(),
            ..Default::default()
        };
        let ready = [
            (
                "Ollama",
                "http://localhost:11434/v1".to_owned(),
                String::new(),
            ),
            (
                crate::on_device::NAME,
                crate::on_device::BASE.to_owned(),
                String::new(),
            ),
        ];
        let accounts = accounts(&primary, &order(&Settings::default()), &ready);
        assert_eq!(accounts.len(), 1);
        assert!(accounts.iter().all(|c| !crate::on_device::at(&c.endpoint)));
    }
}
