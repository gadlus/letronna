//! OpenCode, with one key from its console: its Go plan, a monthly plan for
//! open coding models (GLM, Kimi, Qwen, MiniMax, DeepSeek and others), and
//! Zen, pay-as-you-go from the console's balance, which serves those and
//! more (Claude, GPT, Grok, free ones).
//!
//! One gateway for both, since one key reaches both: a model Go serves goes
//! to Go while the key has a Go plan, and is paid for by it; any other goes
//! to Zen. Go's own "Extra Usage" decides whether a full plan spends the
//! balance, so a Go model is never moved to Zen here. A key with no Go plan
//! sends everything to Zen. Go's limits hold back only what goes to Go
//! (`plan_covers`).
//!
//! Go and Zen take other coding agents than OpenCode on three conditions
//! (opencode.ai/docs/go, "Where can I use it?"): coding-agent traffic, the
//! client's own User-Agent, which `chat::client` sends, and a session id in
//! `x-opencode-session` that stays the same for a conversation, without
//! which Go refuses every request as `MissingSessionID`.
//!
//! Each model is served in one format, at the address OpenCode gives that
//! format (opencode.ai/docs/go and /docs/zen list them, under
//! `/inference/`): chat completions for most; Anthropic's Messages for
//! Claude, some MiniMax and Qwen models; the Responses API for GPT, Grok and
//! Muse Spark; and Google's own for Gemini. Jev speaks a format of its own,
//! SystemOne, which Letronna does not, so it is left out.

use super::usage::{self, Usage, Window};
use super::{AuthKind, Provider, ProviderInfo, Quota, Tier, Wire};
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};

pub(crate) const NAME: &str = "OpenCode Go/Zen";

/// Zen, pay-as-you-go: its list of models, and its chat completions and
/// Responses API, the gateway's own address.
const ZEN: &str = "https://opencode.ai/inference/v1";
const ZEN_OPENAI: &str = "https://opencode.ai/inference/openai/v1";
/// Go, for the models its plan serves: its list and its usage page. Each
/// format has an address of its own under both (`address`).
const GO: &str = "https://opencode.ai/inference/go/v1";

/// How long what Go said of a key and of its models holds.
const KNOWN_FOR: Duration = Duration::from_secs(60 * 60);

pub struct OpenCode;

impl Provider for OpenCode {
    fn info(&self) -> ProviderInfo {
        ProviderInfo {
            name: NAME,
            base: ZEN_OPENAI,
            blurb: "One OpenCode key: open coding models on your Go plan, everything else pay-as-you-go on Zen.",
            docs: "https://opencode.ai/auth",
            tier: Tier::Subscription,
        }
    }

    fn auth(&self) -> AuthKind {
        AuthKind::ApiKey
    }

    fn base_for(&self, key: &str, model: &str) -> Option<String> {
        let go = to_go(key, model);
        let wire = match go {
            true => go_wire(model),
            false => zen_wire(model),
        };
        Some(address(go, wire))
    }

    fn wire_for(&self, key: &str, model: &str) -> Option<Wire> {
        Some(match to_go(key, model) {
            true => go_wire(model),
            false => zen_wire(model),
        })
    }

    fn plan_covers(&self, key: &str, model: &str) -> bool {
        to_go(key, model)
    }

    fn headers(&self, session: &str) -> Vec<(&'static str, String)> {
        vec![("x-opencode-session", session.to_owned())]
    }

    /// Zen's models and, with a Go plan, Go's: each once, Jev left out.
    /// Zen's list is at an address of its own, not under any one format's.
    fn models(&self, base: &str, key: &str) -> Result<Vec<String>, String> {
        let list = match base.trim().trim_end_matches('/') {
            ZEN_OPENAI => ZEN,
            elsewhere => elsewhere,
        };
        let zen = crate::chat::list_models(list, key)?;
        let go = match has_go(key) {
            true => go_models(),
            false => Vec::new(),
        };
        let mut seen = HashSet::new();
        Ok((go.into_iter().chain(zen))
            .filter(|id| speakable(id) && seen.insert(id.clone()))
            .collect())
    }

    /// A refusal for a full Go window says how long until it starts again;
    /// the router takes it only from a reply Go sent (`plan_covers`).
    fn quota(&self, headers: &reqwest::header::HeaderMap) -> Option<Quota> {
        let now = crate::chat::now();
        let wait = crate::limits::retry_after(headers)?;
        Some(Quota {
            plan: None,
            used_percent: 100.0,
            resets_at: Some(now + wait.as_secs()),
            windows: Vec::new(),
        })
    }

    /// Go's usage page: its five-hour, weekly and monthly windows. Zen has
    /// none; it is paid as it goes.
    fn usage(&self, _account: &str, _base: &str, key: &str) -> Result<Option<Usage>, String> {
        match go_usage(key)? {
            Some(body) => Ok(Some(usage_page(&body))),
            None => Ok(Some(Usage {
                note: Some("No Go plan on this key: pay-as-you-go on Zen".into()),
                ..Default::default()
            })),
        }
    }
}

/// Go's usage page for `key`: the reply, `None` for a key with no Go plan
/// on it, or why it could not be read.
fn go_usage(key: &str) -> Result<Option<Value>, String> {
    let reply = crate::chat::client()?
        .get(format!("{GO}/usage"))
        .bearer_auth(key.trim())
        .timeout(Duration::from_secs(15))
        .send()
        .map_err(|_| "Cannot reach OpenCode to read Go's usage.".to_owned())?;
    let status = reply.status().as_u16();
    remember_plan(key, !matches!(status, 401 | 403));
    match status {
        200..=299 => reply
            .json::<Value>()
            .map(Some)
            .map_err(|_| "OpenCode sent usage Letronna cannot read.".to_owned()),
        // A key OpenCode knows that has no Go plan on it.
        403 => Ok(None),
        401 => Err("OpenCode turned the key down.".into()),
        429 => Err("OpenCode asks to wait before Go's usage is read again.".into()),
        status => Err(format!(
            "OpenCode could not give Go's usage (HTTP {status})."
        )),
    }
}

/// Whether `model` goes to Go with `key`: Go serves it, and the key has a
/// Go plan.
fn to_go(key: &str, model: &str) -> bool {
    let model = model.trim();
    has_go(key) && go_models().iter().any(|m| m == model)
}

/// What is known of each key's Go plan, by a hash of the key, and when.
fn plans() -> &'static Mutex<HashMap<u64, (bool, Instant)>> {
    static PLANS: std::sync::OnceLock<Mutex<HashMap<u64, (bool, Instant)>>> =
        std::sync::OnceLock::new();
    PLANS.get_or_init(Default::default)
}

fn hashed(key: &str) -> u64 {
    let mut hash = std::hash::DefaultHasher::new();
    key.trim().hash(&mut hash);
    hash.finish()
}

fn remember_plan(key: &str, go: bool) {
    (plans().lock().unwrap_or_else(PoisonError::into_inner))
        .insert(hashed(key), (go, Instant::now()));
}

/// Whether `key` has a Go plan, as Go's usage page says, asked once an
/// hour. When it cannot be asked the plan is taken to be there: a request
/// that Go then refuses says so, where one sent to Zen instead would be
/// paid for without being asked.
fn has_go(key: &str) -> bool {
    if key.trim().is_empty() {
        return false;
    }
    let known = (plans().lock().unwrap_or_else(PoisonError::into_inner))
        .get(&hashed(key))
        .copied()
        .filter(|(_, at)| at.elapsed() < KNOWN_FOR);
    if let Some((go, _)) = known {
        return go;
    }
    // No test asks OpenCode; one that wants a plan says so (`tests::known`).
    if cfg!(test) {
        return false;
    }
    match go_usage(key) {
        Ok(_) => plans()
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&hashed(key))
            .is_none_or(|(go, _)| *go),
        Err(_) => true,
    }
}

/// Go's list as last asked for, and when.
type Listed = Option<(Vec<String>, Instant)>;

fn go_models_cache() -> std::sync::MutexGuard<'static, Listed> {
    static LISTED: Mutex<Listed> = Mutex::new(None);
    LISTED.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The models Go serves, from its public list, asked once an hour.
fn go_models() -> Vec<String> {
    let mut listed = go_models_cache();
    if let Some((models, at)) = listed.as_ref()
        && at.elapsed() < KNOWN_FOR
    {
        return models.clone();
    }
    if cfg!(test) {
        return listed.as_ref().map(|(m, _)| m.clone()).unwrap_or_default();
    }
    match crate::chat::list_models(GO, "") {
        Ok(models) => {
            *listed = Some((models.clone(), Instant::now()));
            models
        }
        // What was listed before, a little stale, beats nothing.
        Err(_) => listed.as_ref().map(|(m, _)| m.clone()).unwrap_or_default(),
    }
}

/// Where Go, or Zen, takes requests in `wire`'s format.
fn address(go: bool, wire: Wire) -> String {
    let format = match wire {
        Wire::OpenAiChat | Wire::OpenAiResponses => "openai/v1",
        Wire::Anthropic => "anthropic/v1",
        Wire::Google => "google/v1beta",
    };
    let plan = if go { "go/" } else { "" };
    format!("https://opencode.ai/inference/{plan}{format}")
}

/// The format Go serves `model` in, as its table of endpoints lists them.
fn go_wire(model: &str) -> Wire {
    let model = model.trim().to_lowercase();
    let messages = model.starts_with("claude-")
        || model.starts_with("qwen3.")
        || model == "minimax-m3"
        || model.starts_with("minimax-m2");
    match () {
        _ if messages => Wire::Anthropic,
        _ if responses(&model) => Wire::OpenAiResponses,
        _ if gemini(&model) => Wire::Google,
        _ => Wire::OpenAiChat,
    }
}

/// The format Zen serves `model` in: Claude, its free MiniMax models and
/// Qwen but the Max in Messages; as Go, GPT, Grok and Muse in Responses.
fn zen_wire(model: &str) -> Wire {
    let model = model.trim().to_lowercase();
    let messages = model.starts_with("claude-")
        || (model.starts_with("minimax-") && model.ends_with("-free"))
        || (model.starts_with("qwen3.") && !model.contains("-max"));
    match () {
        _ if messages => Wire::Anthropic,
        _ if responses(&model) => Wire::OpenAiResponses,
        _ if gemini(&model) => Wire::Google,
        _ => Wire::OpenAiChat,
    }
}

fn responses(model: &str) -> bool {
    model.starts_with("gpt-") || model.starts_with("grok-") || model.starts_with("muse-")
}

fn gemini(model: &str) -> bool {
    model.starts_with("gemini-")
}

/// Whether Letronna can speak to `model` here: all but Jev, which both
/// serve only in SystemOne's format.
fn speakable(model: &str) -> bool {
    !model.trim().to_lowercase().starts_with("jev-")
}

/// Go's usage page: its five-hour, weekly and monthly windows.
fn usage_page(body: &Value) -> Usage {
    let windows = [
        ("rolling", "Go 5-hour"),
        ("weekly", "Go weekly"),
        ("monthly", "Go monthly"),
    ]
    .into_iter()
    .filter_map(|(key, label)| {
        let window = body.pointer(&format!("/usage/{key}"))?;
        let mut used_percent = usage::percent(&window["percent"])?;
        // Go refuses until the window starts again, whatever the percentage
        // reads.
        if window["status"] == "rate-limited" {
            used_percent = 100.0;
        }
        Some(Window {
            label: label.into(),
            used_percent,
            resets_at: usage::instant(&window["resetsAt"]),
        })
    })
    .collect();
    // The plan's name is the provider's own, so none is given apart.
    Usage {
        windows,
        ..Default::default()
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use serde_json::json;

    /// Take `key` as having a Go plan or not, and Go as serving `models`,
    /// as asking OpenCode would have said.
    pub(crate) fn known(key: &str, go: bool, models: &[&str]) {
        remember_plan(key, go);
        let mut listed = super::go_models_cache();
        *listed = Some((
            models.iter().map(|m| (*m).to_owned()).collect(),
            Instant::now(),
        ));
    }

    #[test]
    fn a_go_model_goes_to_go_with_a_plan_and_everything_else_to_zen() {
        let served = ["glm-5.3", "minimax-m3", "gpt-6-luna", "gemini-4-argon"];
        known("key-go", true, &served);
        known("key-zen", false, &served);
        let p = OpenCode;
        let base = |key: &str, model: &str| p.base_for(key, model).unwrap();
        assert_eq!(
            base("key-go", "glm-5.3"),
            "https://opencode.ai/inference/go/openai/v1"
        );
        assert!(p.plan_covers("key-go", "glm-5.3"));
        // Zen alone serves Claude, and a key with no Go plan sends all there.
        assert_eq!(
            base("key-go", "claude-sonnet-5-5"),
            "https://opencode.ai/inference/anthropic/v1"
        );
        assert!(!p.plan_covers("key-go", "claude-sonnet-5-5"));
        assert_eq!(
            base("key-zen", "glm-5.3"),
            "https://opencode.ai/inference/openai/v1"
        );
        // Gemini, in Google's own format at each one's address for it.
        assert_eq!(
            base("key-go", "gemini-4-argon"),
            "https://opencode.ai/inference/go/google/v1beta"
        );
        assert_eq!(
            base("key-go", "gemini-3.8-flash"),
            "https://opencode.ai/inference/google/v1beta"
        );
        assert_eq!(p.wire_for("key-go", "gemini-4-argon"), Some(Wire::Google));
        assert_eq!(p.wire_for("key-zen", "gemini-3-flash"), Some(Wire::Google));
        // The same model, in the format each serves it in.
        assert_eq!(p.wire_for("key-go", "minimax-m3"), Some(Wire::Anthropic));
        assert_eq!(p.wire_for("key-zen", "minimax-m3"), Some(Wire::OpenAiChat));
        assert_eq!(
            p.wire_for("key-zen", "claude-opus-5-5"),
            Some(Wire::Anthropic)
        );
        assert_eq!(
            p.wire_for("key-zen", "minimax-m3-free"),
            Some(Wire::Anthropic)
        );
        assert_eq!(p.wire_for("key-zen", "qwen3.8-max"), Some(Wire::OpenAiChat));
        assert_eq!(p.wire_for("key-zen", "qwen3.6-plus"), Some(Wire::Anthropic));
        assert_eq!(
            p.wire_for("key-go", "gpt-6-luna"),
            Some(Wire::OpenAiResponses)
        );
        assert_eq!(
            p.wire_for("key-zen", "grok-4.7"),
            Some(Wire::OpenAiResponses)
        );
        assert_eq!(p.wire_for("key-zen", "kimi-k3"), Some(Wire::OpenAiChat));
        assert!(!speakable("jev-1.13") && speakable("gemini-4-argon"));
    }

    #[test]
    fn every_request_names_its_session() {
        assert_eq!(
            OpenCode.headers("letronna-7"),
            [("x-opencode-session", "letronna-7".to_owned())]
        );
    }

    #[test]
    fn the_go_plans_three_windows_come_from_its_usage_page() {
        let page = json!({"usage": {
            "rolling": {"percent": 12.5, "resetsAt": "2026-10-27T12:00:00.000Z"},
            "weekly": {"percent": 40, "resetsAt": "2026-10-30T00:00:00.000Z", "status": "ok"},
            "monthly": {"percent": 80, "resetsAt": "2026-11-01T00:00:00.000Z", "status": "rate-limited"},
        }});
        let usage = usage_page(&page);
        let labels: Vec<&str> = usage.windows.iter().map(|w| w.label.as_str()).collect();
        assert_eq!(labels, ["Go 5-hour", "Go weekly", "Go monthly"]);
        assert_eq!(usage.windows[0].resets_at, Some(1_793_102_400));
        assert_eq!(usage.windows[2].used_percent, 100.0);
    }

    #[test]
    fn a_refusal_for_a_full_window_holds_the_plan_back_till_it_says() {
        let headers: reqwest::header::HeaderMap = [("retry-after", "120")]
            .into_iter()
            .map(|(k, v)| (k.parse().unwrap(), v.parse().unwrap()))
            .collect();
        let quota = OpenCode.quota(&headers).unwrap();
        assert_eq!(quota.used_percent, 100.0);
        assert!(quota.resets_at.unwrap() >= crate::chat::now() + 119);
        assert!(OpenCode.quota(&Default::default()).is_none());
    }
}
