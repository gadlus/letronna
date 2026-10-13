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
use super::{AuthKind, Provider, ProviderInfo, Tier, Wire};

pub(crate) const NAME: &str = "Claude";

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

    fn caveat(&self) -> Option<&'static str> {
        Some(
            "Letronna presents itself to Anthropic as the client your subscription's \
             token was issued for, which is what makes the subscription usable outside \
             that client. Anthropic may change what it expects, or object.",
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
