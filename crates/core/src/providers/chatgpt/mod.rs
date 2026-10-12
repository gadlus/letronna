//! A ChatGPT plan, signed in to in the browser, in place of an API key:
//! the models the plan includes for Codex, from ChatGPT's backend.
//!
//! The sign-in is OpenAI's own, the one Codex uses, and only comes back to
//! `localhost:1455`. The backend speaks the Responses API alone, streamed
//! only and with nothing stored; what it needs beyond OpenAI's own API is
//! added at the edge, in `wire::openai_responses`.

use super::auth::OAuthSpec;
use super::{AuthKind, Provider, ProviderInfo, Quota, Tier, Wire};
use serde_json::{Value, json};
use std::time::Duration;

pub(crate) const NAME: &str = "ChatGPT";

/// The window plugin that offers it, by its id in `plugin-switches.json`.
const PLUGIN: &str = "chatgpt";

static SIGN_IN: OAuthSpec = OAuthSpec {
    authorize: "https://auth.openai.com/oauth/authorize",
    token: "https://auth.openai.com/oauth/token",
    // The client OpenAI's sign-in knows Codex as.
    client_id: "app_EMoamEEZ73f0CkXaXp7hrann",
    client_secret: None,
    scopes: "openid profile email offline_access",
    // OpenAI sends this client's sign-in back here and nowhere else.
    port: 1455,
    path: "/auth/callback",
    extra: &[
        ("id_token_add_organizations", "true"),
        ("codex_cli_simplified_flow", "true"),
        ("originator", "letronna"),
    ],
    json: false,
    pkce: true,
    state_in_token: false,
    account_claim: Some("/https:~1~1api.openai.com~1auth/chatgpt_account_id"),
    plan_claim: Some("/https:~1~1api.openai.com~1auth/chatgpt_plan_type"),
};

/// The Codex version the model list is asked for as. Each model names the
/// oldest Codex whose own tools it needs; Letronna sends the plain Responses
/// API, which every model takes, so it asks as one newer than any.
const CLIENT_VERSION: &str = "99.0.0";

/// The efforts the Responses API takes. Codex's own ultra, which hands parts
/// of a task to other agents, is not one of them.
const EFFORTS: [(&str, &str); 7] = [
    ("none", "None"),
    ("minimal", "Minimal"),
    ("low", "Low"),
    ("medium", "Medium"),
    ("high", "High"),
    ("xhigh", "Extra high"),
    ("max", "Max"),
];

/// The switch, kept beside the plugin's own, that has ChatGPT use the
/// Codex app's sign-in instead of one of its own.
pub const CODEX_SWITCH: &str = "chatgpt.codex-sign-in";

/// What a refused shared sign-in says: Letronna never renews it itself.
pub const RENEW_IN_CODEX: &str =
    "ChatGPT refused the Codex app's sign-in. Use Codex once so it renews its sign-in.";

/// Whether ChatGPT uses the Codex app's sign-in.
pub fn shares_codex() -> bool {
    super::switches().get(CODEX_SWITCH) == Some(&true)
}

/// Where the Codex app keeps its sign-in.
fn codex_file() -> Option<std::path::PathBuf> {
    let home = std::env::var_os("CODEX_HOME")
        .map(std::path::PathBuf::from)
        .or_else(|| crate::tools::home().map(|home| home.join(".codex")))?;
    Some(home.join("auth.json"))
}

/// The Codex app's sign-in, read afresh on every request so a renewal there
/// is picked up at once. Only read: never refreshed and never written, so
/// Codex's own copy is never changed under it.
pub(crate) fn codex_tokens() -> Option<super::auth::Tokens> {
    let saved: Value = serde_json::from_slice(&std::fs::read(codex_file()?).ok()?).ok()?;
    shared(&saved["tokens"], super::auth::now())
}

/// `tokens` from Codex's `auth.json` as a sign-in Letronna will not renew:
/// no refresh token, no end to refresh ahead of, refreshed just now.
fn shared(tokens: &Value, now: u64) -> Option<super::auth::Tokens> {
    let mut held = super::auth::from_reply(&SIGN_IN, tokens, None).ok()?;
    if let Some(account) = tokens["account_id"].as_str().filter(|a| !a.is_empty()) {
        held.account = account.to_owned();
    }
    held.refresh.clear();
    held.expires_at = 0;
    held.refreshed_at = now;
    Some(held)
}

pub struct ChatGpt;

impl Provider for ChatGpt {
    fn info(&self) -> ProviderInfo {
        ProviderInfo {
            name: NAME,
            base: "https://chatgpt.com/backend-api/codex",
            blurb: "Opt-in: sign in with your ChatGPT plan instead of an API key. Uses its Codex models and limits.",
            docs: "https://chatgpt.com/codex",
            tier: Tier::Subscription,
        }
    }

    fn auth(&self) -> AuthKind {
        AuthKind::OAuth(&SIGN_IN)
    }

    fn caveat(&self) -> Option<&'static str> {
        Some("Using your ChatGPT plan here may be subject to OpenAI's terms.")
    }

    fn plugin(&self) -> Option<&'static str> {
        Some(PLUGIN)
    }

    fn wire(&self) -> Wire {
        Wire::OpenAiResponses
    }

    /// The plan's own limits, which its backend sends on every reply: the
    /// share of each window used and when it starts again. The tighter is
    /// the quota.
    fn quota(&self, headers: &reqwest::header::HeaderMap) -> Option<Quota> {
        plan_usage(headers, crate::chat::now())
    }

    fn models(&self, base: &str, key: &str) -> Result<Vec<String>, String> {
        let base = base.trim().trim_end_matches('/');
        let request = crate::chat::client()?
            .get(format!("{base}/models?client_version={CLIENT_VERSION}"))
            .timeout(Duration::from_secs(20));
        let response = super::wire::openai_responses::as_chatgpt(request, key)
            .send()
            .and_then(|r| r.error_for_status())
            .map_err(|e| match e.status().map(|s| s.as_u16()) {
                Some(401 | 403) if shares_codex() => RENEW_IN_CODEX.to_owned(),
                Some(401 | 403) => "ChatGPT refused the sign-in. Sign in again.".to_owned(),
                _ => "Cannot load the model list from ChatGPT.".to_owned(),
            })?;
        let catalog: Value = response
            .json()
            .map_err(|_| "ChatGPT sent a model list Letronna cannot read.".to_owned())?;
        let list = listing(&catalog);
        crate::models::remember(base, &list);
        Ok(list
            .iter()
            .filter_map(|m| m["id"].as_str().map(str::to_owned))
            .collect())
    }
}

/// The models ChatGPT lists for the plan, in its order, as entries of a
/// `/models` list with their window, inputs and the efforts the Responses
/// API takes. Those it keeps for Codex's own work are hidden and left out.
fn listing(catalog: &Value) -> Vec<Value> {
    let mut listed: Vec<&Value> = catalog["models"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|model| model["visibility"] == "list" && model["slug"].is_string())
        .collect();
    listed.sort_by_key(|model| model["priority"].as_i64().unwrap_or(i64::MAX));
    listed
        .into_iter()
        .map(|model| {
            let efforts: Vec<Value> = model["supported_reasoning_levels"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|level| {
                    let effort = level["effort"].as_str()?;
                    let (_, label) = EFFORTS.iter().find(|(known, _)| *known == effort)?;
                    Some(json!({"effort": effort, "label": label}))
                })
                .collect();
            let mut entry = json!({
                "id": model["slug"],
                "object": "model",
                "owned_by": "openai",
                "name": model["display_name"],
                "context_length": model["context_window"],
                "input_modalities": model["input_modalities"],
            });
            if efforts.is_empty() {
                entry["capabilities"] = json!({"reasoning": false});
            } else {
                entry["reasoning_efforts"] = efforts.into();
            }
            entry
        })
        .collect()
}

/// The tighter of the plan's two windows at `now`, as its headers give them.
fn plan_usage(headers: &reqwest::header::HeaderMap, now: u64) -> Option<Quota> {
    let number = |name: String| headers.get(name)?.to_str().ok()?.trim().parse::<f64>().ok();
    let (used_percent, reset) = (["primary", "secondary"].into_iter())
        .filter_map(|window| {
            let used = number(format!("x-codex-{window}-used-percent"))?;
            Some((
                used,
                number(format!("x-codex-{window}-reset-after-seconds")),
            ))
        })
        .max_by(|a, b| a.0.total_cmp(&b.0))?;
    Some(Quota {
        plan: super::auth::tokens(NAME)
            .map(|t| t.plan)
            .filter(|plan| !plan.is_empty()),
        used_percent,
        resets_at: reset.map(|after| now + after.max(0.0) as u64),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_codex_apps_sign_in_is_taken_as_one_never_to_renew() {
        let claims = json!({"https://api.openai.com/auth": {"chatgpt_plan_type": "plus"}});
        let jwt = |c: &Value| {
            let part = |v: &Value| super::super::auth::base64url(v.to_string().as_bytes());
            format!("{}.{}.sig", part(&json!({"alg": "none"})), part(c))
        };
        let saved = json!({
            "id_token": jwt(&claims), "access_token": "access-x",
            "refresh_token": "refresh-x", "account_id": "acct-1",
        });
        let held = shared(&saved, 500).unwrap();
        assert_eq!(
            (held.access.as_str(), held.account.as_str()),
            ("access-x", "acct-1")
        );
        assert_eq!(held.plan, "plus");
        assert!(held.refresh.is_empty() && held.expires_at == 0 && held.refreshed_at == 500);
        assert!(shared(&json!({}), 0).is_none());
    }

    #[test]
    fn the_plans_tighter_window_is_its_quota() {
        let headers: reqwest::header::HeaderMap = [
            ("x-codex-primary-used-percent", "12.5"),
            ("x-codex-primary-reset-after-seconds", "600"),
            ("x-codex-secondary-used-percent", "40"),
            ("x-codex-secondary-reset-after-seconds", "86400"),
        ]
        .into_iter()
        .map(|(k, v)| (k.parse().unwrap(), v.parse().unwrap()))
        .collect();
        let quota = plan_usage(&headers, 100).unwrap();
        assert_eq!((quota.used_percent, quota.resets_at), (40.0, Some(86_500)));
        assert_eq!(plan_usage(&Default::default(), 0), None);
    }

    #[test]
    fn only_the_models_the_plan_lists_are_offered_with_the_efforts_the_api_takes() {
        let level = |effort: &str| json!({"effort": effort, "description": ""});
        let second = json!({
            "slug": "second", "visibility": "list", "priority": 5,
            "context_window": 272_000, "input_modalities": ["text", "image"],
            "supported_reasoning_levels": [level("low"), level("max"), level("ultra")],
        });
        let hidden = json!({"slug": "hidden", "visibility": "hide", "priority": 1});
        let first = json!({
            "slug": "first", "visibility": "list", "priority": 2,
            "context_window": 128_000, "supported_reasoning_levels": [],
        });
        let list = listing(&json!({"models": [second, hidden, first]}));
        let ids: Vec<&str> = list.iter().map(|m| m["id"].as_str().unwrap()).collect();
        assert_eq!(ids, ["first", "second"]);
        assert_eq!(list[1]["context_length"], 272_000);
        assert_eq!(
            list[1]["reasoning_efforts"],
            json!([{"effort": "low", "label": "Low"}, {"effort": "max", "label": "Max"}])
        );
        assert_eq!(list[0]["capabilities"]["reasoning"], false);
    }
}
