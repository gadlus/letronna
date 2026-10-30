//! A Claude subscription, signed in to in the browser, in place of an API
//! key: the models the plan includes, spoken in Messages format
//! (`wire::anthropic`).
//!
//! The sign-in is Claude Code's own, and three things about its authorize
//! request are not guessable -- each one answers with a bare "Invalid request
//! format" and no redirect if it is wrong:
//!
//!   * the scopes are EXACTLY these three, and adding the ones that look like
//!     they belong (`user:sessions:claude_code`, `user:mcp_servers`) is
//!     refused;
//!   * `scope` must arrive form-encoded, so its separators are `+` and not
//!     `%20`, which `authorize_url` gives by building with
//!     `Url::parse_with_params`;
//!   * the only extra parameter is `code=true` -- not `prompt=login`.
//!
//! Its token endpoint takes JSON, and **wants the state back**, which most do
//! not: a stateless exchange is answered with a bare 400. That is
//! `state_in_token`.
//!
//! What it needs beyond Anthropic's own API is added at the edge, in
//! `wire::anthropic`: a token issued to Claude Code is expected to arrive
//! with the marks that client always sends, and a request without them is
//! refused as a rate limit on an account that is not rate limited. See
//! `cloak`.

pub(crate) mod cloak;

use super::auth::OAuthSpec;
use super::usage::{self, Usage, Window};
use super::{AuthKind, Provider, ProviderInfo, Quota, Tier, Wire};
use serde_json::Value;

pub(crate) const NAME: &str = "Claude";

/// The window plugin that offers it, by its id in `plugin-switches.json`.
const PLUGIN: &str = "claude";

static SIGN_IN: OAuthSpec = OAuthSpec {
    authorize: "https://claude.ai/oauth/authorize",
    token: "https://api.anthropic.com/v1/oauth/token",
    // The client Claude's sign-in knows Claude Code as.
    client_id: "9d1c250a-e61b-44d9-88ed-5944d1962f5e",
    client_secret: None,
    scopes: "org:create_api_key user:profile user:inference",
    // Any free one: this client takes whichever loopback port it is given.
    port: 0,
    path: "/callback",
    extra: &[("code", "true")],
    json: true,
    pkce: true,
    // Required here, unlike most: see the note above.
    state_in_token: true,
    // Its tokens carry no id token and no claims to read these from; who the
    // account is comes from the bootstrap call instead.
    account_claim: None,
    plan_claim: None,
    device: None,
    // Claude Code's own page for a sign-in finished elsewhere: it shows
    // `code#state` to paste back, which `code=true` above asks for.
    manual_redirect: Some("https://platform.claude.com/oauth/code/callback"),
};

pub struct Claude;

impl Provider for Claude {
    fn info(&self) -> ProviderInfo {
        ProviderInfo {
            name: NAME,
            base: "https://api.anthropic.com/v1",
            blurb: "Your Claude subscription. Sign in; no key to paste.",
            docs: "https://claude.ai/settings/profile",
            tier: Tier::Subscription,
        }
    }

    fn auth(&self) -> AuthKind {
        AuthKind::OAuth(&SIGN_IN)
    }

    fn wire(&self) -> Wire {
        Wire::Anthropic
    }

    /// Asked of Anthropic with the headers a subscription token needs, which
    /// the plain `GET <base>/models` does not send.
    fn models(&self, base: &str, key: &str) -> Result<Vec<String>, String> {
        let base = base.trim().trim_end_matches('/');
        let reply = crate::chat::client()?
            .get(format!("{base}/models?limit=100"))
            .timeout(std::time::Duration::from_secs(20))
            .bearer_auth(key.trim())
            .header("anthropic-version", "2023-06-01")
            .header("anthropic-beta", cloak::OAUTH_BETA)
            .header("user-agent", cloak::USER_AGENT)
            .header("x-app", "cli")
            .send()
            .and_then(reqwest::blocking::Response::error_for_status)
            .map_err(|e| match e.status().map(|s| s.as_u16()) {
                Some(401) | Some(403) => "Claude turned the sign-in down.".to_owned(),
                _ => "Cannot load the model list from Claude.".to_owned(),
            })?;
        let body: serde_json::Value = reply.json().map_err(|_| "Invalid model list.".to_owned())?;
        Ok(body["data"]
            .as_array()
            .ok_or("Invalid model list.")?
            .iter()
            .filter_map(|row| row["id"].as_str())
            .map(str::to_owned)
            .collect())
    }

    /// A subscription's replies name its own windows, not the API's
    /// request and token limits; an API key's replies on this address are
    /// read as Anthropic's are.
    fn quota(&self, headers: &reqwest::header::HeaderMap) -> Option<Quota> {
        plan_limits(headers, crate::chat::now())
            .or_else(|| super::router::rate_limits(headers, crate::chat::now()))
    }

    /// The page Claude Code's `/usage` reads: each window of the plan.
    fn usage(&self, _account: &str, base: &str, key: &str) -> Result<Option<Usage>, String> {
        let root = base.trim().trim_end_matches('/');
        let root = root.strip_suffix("/v1").unwrap_or(root);
        let request = crate::chat::client()?
            .get(format!("{root}/api/oauth/usage"))
            .bearer_auth(key.trim())
            .header("anthropic-beta", cloak::OAUTH_BETA)
            .header("user-agent", cloak::USER_AGENT);
        let body = usage::ask(request, "Claude")?;
        Ok(body.map(|body| usage_page(&body)))
    }

    fn caveat(&self) -> Option<&'static str> {
        Some(
            "Letronna presents itself to Anthropic as the client your subscription's \
             token was issued for, which is what makes the subscription usable outside \
             that client. Anthropic may change what it expects, or object.",
        )
    }

    fn plugin(&self) -> Option<&'static str> {
        Some(PLUGIN)
    }
}

/// The plan's windows as a subscription's reply names them: how much of
/// each is used, from 0 to 1, and when each starts again.
fn plan_limits(headers: &reqwest::header::HeaderMap, now: u64) -> Option<Quota> {
    let get = |name: String| headers.get(name)?.to_str().ok().map(str::trim);
    let windows: Vec<Window> = [("5h", "5-hour"), ("7d", "Weekly")]
        .into_iter()
        .filter_map(|(window, label)| {
            let used = get(format!("anthropic-ratelimit-unified-{window}-utilization"))?
                .parse::<f64>()
                .ok()?;
            let resets_at = get(format!("anthropic-ratelimit-unified-{window}-reset"))
                .and_then(|at| usage::instant(&Value::from(at)));
            Some(Window {
                label: label.into(),
                used_percent: (used * 100.0).clamp(0.0, 100.0),
                resets_at,
            })
        })
        .collect();
    let mut tightest = windows
        .iter()
        .max_by(|a, b| a.used_percent.total_cmp(&b.used_percent))?
        .clone();
    // A refusal says so outright, whatever the windows read.
    if get("anthropic-ratelimit-unified-status".into()) == Some("rejected") {
        tightest.used_percent = 100.0;
        tightest.resets_at = get("anthropic-ratelimit-unified-reset".into())
            .and_then(|at| usage::instant(&Value::from(at)))
            .or(tightest.resets_at)
            .filter(|&at| at > now);
    }
    Some(Quota {
        plan: None,
        used_percent: tightest.used_percent,
        resets_at: tightest.resets_at,
        windows,
    })
}

/// Claude's usage page: the 5-hour and weekly windows, used from 0 to 100,
/// and any weekly window of a model of its own.
fn usage_page(body: &Value) -> Usage {
    let window = |key: &str, label: String| {
        let at = &body[key];
        Some(Window {
            label,
            used_percent: usage::percent(&at["utilization"])?,
            resets_at: usage::instant(&at["resets_at"]),
        })
    };
    let mut windows: Vec<Window> = [
        window("five_hour", "5-hour".into()),
        window("seven_day", "Weekly".into()),
        window("seven_day_opus", "Weekly · Opus".into()),
        window("seven_day_sonnet", "Weekly · Sonnet".into()),
    ]
    .into_iter()
    .flatten()
    .collect();
    for scoped in body["model_scoped"].as_array().into_iter().flatten() {
        let (Some(name), Some(used)) = (
            scoped["display_name"].as_str(),
            usage::percent(&scoped["utilization"]),
        ) else {
            continue;
        };
        let label = format!("Weekly · {name}");
        if !windows.iter().any(|w| w.label == label) {
            windows.push(Window {
                label,
                used_percent: used,
                resets_at: usage::instant(&scoped["resets_at"]),
            });
        }
    }
    Usage {
        windows,
        ..Default::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_subscriptions_windows_are_read_from_its_replies_and_its_page() {
        let headers: reqwest::header::HeaderMap = [
            ("anthropic-ratelimit-unified-5h-utilization", "0.62"),
            ("anthropic-ratelimit-unified-5h-reset", "1793110000"),
            ("anthropic-ratelimit-unified-7d-utilization", "0.2"),
            ("anthropic-ratelimit-unified-7d-reset", "1793500000"),
            ("anthropic-ratelimit-unified-status", "allowed"),
        ]
        .into_iter()
        .map(|(k, v)| (k.parse().unwrap(), v.parse().unwrap()))
        .collect();
        let quota = plan_limits(&headers, 1_793_100_000).unwrap();
        assert_eq!(quota.windows.len(), 2);
        assert_eq!(quota.windows[0].label, "5-hour");
        assert!((quota.used_percent - 62.0).abs() < 1e-9);
        assert_eq!(quota.resets_at, Some(1_793_110_000));
        assert!(plan_limits(&Default::default(), 0).is_none());

        let page = serde_json::json!({
            "five_hour": {"utilization": 23.0, "resets_at": "2026-10-27T12:00:00+00:00"},
            "seven_day": {"utilization": 41, "resets_at": "2026-10-30T09:00:00+00:00"},
            "seven_day_opus": null,
            "model_scoped": [{"display_name": "Opus", "utilization": 70, "resets_at": null}],
            "extra_usage": {"is_enabled": false},
        });
        let usage = usage_page(&page);
        let labels: Vec<&str> = usage.windows.iter().map(|w| w.label.as_str()).collect();
        assert_eq!(labels, ["5-hour", "Weekly", "Weekly · Opus"]);
        assert_eq!(usage.windows[0].resets_at, Some(1_793_102_400));
        assert_eq!(usage.windows[2].used_percent, 70.0);
    }

    /// Each of these is a bare "Invalid request format" with no redirect when
    /// it is wrong, so they are pinned rather than trusted.
    #[test]
    fn the_sign_in_is_the_shape_claude_accepts() {
        assert_eq!(
            SIGN_IN.scopes,
            "org:create_api_key user:profile user:inference"
        );
        assert!(!SIGN_IN.scopes.contains("sessions"));
        assert!(!SIGN_IN.scopes.contains("mcp_servers"));
        assert_eq!(SIGN_IN.extra, &[("code", "true")]);
        assert!(SIGN_IN.json, "its token endpoint takes JSON");
        assert!(SIGN_IN.state_in_token, "a stateless exchange is a bare 400");
        assert!(SIGN_IN.pkce);
        assert_eq!(SIGN_IN.port, 0, "any free loopback port");
    }

    #[test]
    fn it_is_a_subscription_spoken_in_messages_format() {
        assert_eq!(Claude.wire(), Wire::Anthropic);
        assert_eq!(Claude.info().tier, Tier::Subscription);
        assert!(matches!(Claude.auth(), AuthKind::OAuth(_)));
        // Nothing to paste, so nothing says to paste it.
        assert!(!Claude.info().blurb.to_lowercase().contains("api key"));
    }
}
