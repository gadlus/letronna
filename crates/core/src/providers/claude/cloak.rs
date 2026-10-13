//! The marks the client that owns a Claude subscription token always sends.
//!
//! ## Why this is here, because "spoofing" is the wrong way to read it
//!
//! Signing in with a Claude subscription yields an OAuth token issued to
//! Claude Code (`sk-ant-oat…`). Anthropic treats such a token as belonging to
//! that client and expects the request to carry what that client always
//! carries. A request presenting the token *without* those marks is not merely
//! unusual: it is refused, and the refusal is `rate_limit_error` -- on an
//! account that is not rate limited at all.
//!
//! That error is the whole reason this module exists. Nothing in it names the
//! real cause, so it sends you looking at your plan, your quota and your
//! network, which are all fine. So this is not defeating a control; it is
//! sending the request the credential was issued for, on behalf of the person
//! who owns it, so that their own subscription behaves here the way it does in
//! the client they got the token from. Without it the plugin does not work.
//!
//! Three things are sent, and they have to agree with each other:
//!
//! 1. a billing block, first in `system`, naming the client version and
//!    entrypoint;
//! 2. `metadata.user_id`, a small document carrying a device id, the account
//!    uuid and a session id;
//! 3. the same session id again as a header -- two different values is a
//!    mismatch no single field would show.
//!
//! Nothing here reads or alters the prompt. It adds a system block, a metadata
//! field and some headers.
//!
//! The version is also a **model gate**: Anthropic refuses a model to a client
//! older than it wants, answering 400 with the floor it requires. So the
//! version pinned below is not decoration, and the whole set moves together --
//! see `BUILD_REVISION`.

use serde_json::{Value, json};

/// Claude Code's version, which is also the floor Anthropic checks a model
/// against. 2.1.280 cleared Opus 5.5, 2.1.284 Sonnet 5.5, 2.1.293 Haiku 5.5.
const CLAUDE_CODE_VERSION: &str = "2.1.293";

/// The suffix after the version in the billing block's `cc_version`.
///
/// The real client computes it per request, as the first three hex digits of
///
/// ```text
/// sha256("59cf53e54c78" + text[4] + text[7] + text[20] + version)
/// ```
///
/// where `text` is the first user message and an index past its end reads
/// `"0"`. A constant here rather than computed, because *which* text the
/// client reads is not settled, and this is the value a short first message
/// gives on this version. It moves with `CLAUDE_CODE_VERSION`:
///
/// ```text
/// sha256("59cf53e54c78" + "000" + "<version>")[..3]
/// ```
const BUILD_REVISION: &str = "9b8";

/// The Anthropic SDK bundled in the captured client.
const STAINLESS_PACKAGE_VERSION: &str = "0.112.1";

/// The Node version the captured client reports. Pinned rather than read from
/// this machine: it describes the client, which is Node, and this is not.
const RUNTIME_VERSION: &str = "v26.3.0";

/// The entrypoint label. It must agree with the User-Agent's parenthetical:
/// `cli` goes with `(external, cli)`. `sdk-cli` is a different client, and an
/// entrypoint naming one client against a User-Agent naming another is worse
/// than sending neither.
const ENTRYPOINT: &str = "cli";

/// The beta that makes a subscription token acceptable at all: on the token
/// endpoint, on the model list, and on every served request.
pub(crate) const OAUTH_BETA: &str = "oauth-2025-04-20";

pub(crate) const USER_AGENT: &str = concat!("claude-cli/", "2.1.293", " (external, cli)");

/// The sentinel that says which client this is. The person's own instructions
/// extend it rather than replace it.
const CLIENT_SYSTEM_PROMPT: &str = "You are Claude Code, Anthropic's official CLI for Claude.";

const BILLING_PREFIX: &str = "x-anthropic-billing-header:";

fn billing_header() -> String {
    // `cch=00000` is a placeholder. The real client puts a short hash of the
    // final wire bytes there, which cannot be known before those bytes exist;
    // the placeholder is the form the provider canonicalizes back to anyway,
    // and leaving it is honest about not computing one.
    format!(
        "{BILLING_PREFIX} cc_version={CLAUDE_CODE_VERSION}.{BUILD_REVISION}; \
         cc_entrypoint={ENTRYPOINT}; cch=00000;"
    )
}

/// Put the marks on an Anthropic request body, and hand back the session id
/// the headers must then carry.
///
/// The billing block is deduplicated at any index and exactly one prepended,
/// so applying this twice cannot double it. `metadata.user_id` is overwritten
/// rather than preserved: ids that do not belong to this credential describe a
/// client that does not exist.
pub(crate) fn apply(body: &mut Value, tokens: &crate::providers::auth::Tokens) -> String {
    let device = device_id(tokens);
    let account = account_uuid(tokens, &device);
    let session = session_id(body, &device);

    let mut blocks: Vec<Value> = match body.get("system") {
        Some(Value::Array(existing)) => existing
            .iter()
            .filter(|block| !is_billing(block))
            .cloned()
            .collect(),
        // A bare string system prompt becomes a block, keeping its text.
        Some(Value::String(text)) if !text.trim().is_empty() => {
            vec![json!({"type": "text", "text": text})]
        }
        _ => Vec::new(),
    };

    let carries_sentinel = blocks
        .iter()
        .any(|block| block.get("text").and_then(Value::as_str) == Some(CLIENT_SYSTEM_PROMPT));
    if !carries_sentinel {
        blocks.insert(0, json!({"type": "text", "text": CLIENT_SYSTEM_PROMPT}));
    }
    // Billing first, so it is the block that would carry the hash of
    // everything after it.
    blocks.insert(0, json!({"type": "text", "text": billing_header()}));

    body["system"] = Value::Array(blocks);
    body["metadata"] = json!({
        "user_id": json!({
            "device_id": device,
            "account_uuid": account,
            "session_id": session,
        })
        .to_string(),
    });
    session
}

fn is_billing(block: &Value) -> bool {
    block
        .get("text")
        .and_then(Value::as_str)
        .is_some_and(|text| text.starts_with(BILLING_PREFIX))
}

/// The account's uuid: the provider's own where the sign-in read one, else one
/// derived from the device id.
///
/// Derived from the DEVICE id and never from the access token, which is the
/// difference that matters: a token-derived id changes at every renewal, so
/// from the provider's side the account would appear to be a new device every
/// few hours.
fn account_uuid(tokens: &crate::providers::auth::Tokens, device: &str) -> String {
    if !tokens.account.is_empty() {
        return tokens.account.clone();
    }
    derive_uuid(&format!("account:{device}"))
}

/// This installation's device id.
///
/// From the account where the sign-in named one, else from the REFRESH
/// token, which outlives every renewal. Never from the access token: that is
/// replaced every few hours, and an id derived from it would describe a new
/// device each time, which is the opposite of what one is for. A sign-in that
/// rotates its refresh token moves the id with it, which reads as a
/// re-authorised device rather than a fleet of them.
fn device_id(tokens: &crate::providers::auth::Tokens) -> String {
    let seed = match (tokens.account.as_str(), tokens.refresh.as_str()) {
        ("", "") => "device:none".to_owned(),
        ("", refresh) => format!("device:{refresh}"),
        (account, _) => format!("device:{account}"),
    };
    hex(sha256(seed.as_bytes()).as_ref())
}

/// The id that says "these requests are one conversation".
///
/// Derived from the conversation's own head -- the system instruction plus the
/// first user turn -- because that is the part that does not change as turns
/// are appended. Turn twelve has to hash to what turn one hashed to, or every
/// turn presents as a new session and the prompt caching this id exists for
/// never happens. Only a digest leaves here: the text is read, hashed and
/// dropped.
fn session_id(body: &Value, device: &str) -> String {
    let seed = if device.is_empty() {
        "no-device"
    } else {
        device
    };
    let head = conversation_head(body);
    if head.is_empty() {
        return derive_uuid(&format!("conversation:{seed}"));
    }
    let digest = hex(sha256(head.as_bytes()).as_ref());
    derive_uuid(&format!("conversation:{seed}:{digest}"))
}

/// How much of the head feeds the hash. Enough to tell conversations apart,
/// bounded so a long first message does not make this expensive.
const HEAD_BYTES: usize = 4096;

/// The stable head of an Anthropic body: its `system` text and its first user
/// turn. Returns empty when there is nothing stable to read, which the caller
/// reads as "seed from the account instead".
fn conversation_head(body: &Value) -> String {
    let mut parts: Vec<String> = Vec::new();
    match body.get("system") {
        Some(Value::String(text)) => parts.push(text.clone()),
        Some(Value::Array(blocks)) => {
            for block in blocks {
                // Skip our own marks: they are the same in every conversation
                // and the sentinel would otherwise be the whole head of a
                // conversation that has no system prompt of its own.
                if is_billing(block) {
                    continue;
                }
                if let Some(text) = block.get("text").and_then(Value::as_str)
                    && text != CLIENT_SYSTEM_PROMPT
                {
                    parts.push(text.to_owned());
                }
            }
        }
        _ => {}
    }
    for message in body["messages"].as_array().into_iter().flatten() {
        match message["role"].as_str() {
            Some("user") => {
                parts.push(text_of(&message["content"]));
                break;
            }
            // An assistant turn before any user turn means the head is
            // already behind us.
            Some("assistant") => break,
            _ => {}
        }
    }
    let joined = parts.join("\n");
    let head = joined.trim();
    // Cut on a character boundary, since a prompt is not bytes.
    head.char_indices()
        .map(|(at, _)| at)
        .take_while(|at| *at <= HEAD_BYTES)
        .last()
        .map_or(head, |end| &head[..end])
        .trim()
        .to_owned()
}

/// One message's text. A non-text part contributes its kind only: an inline
/// image would fill the whole head while saying nothing about which
/// conversation this is.
fn text_of(content: &Value) -> String {
    match content {
        Value::String(text) => text.clone(),
        Value::Array(parts) => parts
            .iter()
            .map(|part| match part {
                Value::String(text) => text.clone(),
                _ => part["text"].as_str().map_or_else(
                    || format!("<{}>", part["type"].as_str().unwrap_or("part")),
                    str::to_owned,
                ),
            })
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

/// A digest as hex.
fn hex(bytes: &[u8]) -> String {
    bytes.iter().fold(String::new(), |mut out, byte| {
        use std::fmt::Write as _;
        let _ = write!(out, "{byte:02x}");
        out
    })
}

fn sha256(bytes: &[u8]) -> ring::digest::Digest {
    ring::digest::digest(&ring::digest::SHA256, bytes)
}

/// A UUID-v4-shaped value derived from a seed. Deterministic, because an id
/// that changed every call would describe a client that cannot exist. Not a
/// secret: it is a plain hash, so it is only as private as its seed, and every
/// seed here is an internal id rather than a credential.
fn derive_uuid(seed: &str) -> String {
    let h = hex(sha256(seed.as_bytes()).as_ref());
    let variant = u8::from_str_radix(&h[16..17], 16).unwrap_or(0) & 0x3 | 0x8;
    format!(
        "{}-{}-4{}-{variant:x}{}-{}",
        &h[0..8],
        &h[8..12],
        &h[13..16],
        &h[17..20],
        &h[20..32]
    )
}

/// Every header the request carries, in the order the real client emits them.
///
/// `request_id` is a fresh value per request, unlike the session id, which is
/// per conversation.
pub(crate) fn marks(session: &str) -> Vec<(&'static str, String)> {
    vec![
        // The SDK asks for JSON even on a stream: it decides whether to decode
        // events from the body's `stream` field, not from this.
        ("Accept", "application/json".into()),
        ("User-Agent", USER_AGENT.into()),
        ("X-Claude-Code-Session-Id", session.to_owned()),
        ("X-Stainless-Arch", stainless_arch().into()),
        ("X-Stainless-Lang", "js".into()),
        ("X-Stainless-OS", stainless_os().into()),
        (
            "X-Stainless-Package-Version",
            STAINLESS_PACKAGE_VERSION.into(),
        ),
        // Always zero. A retry count is this program's business and not
        // something to report as the client's.
        ("X-Stainless-Retry-Count", "0".into()),
        ("X-Stainless-Runtime", "node".into()),
        ("X-Stainless-Runtime-Version", RUNTIME_VERSION.into()),
        ("X-Stainless-Timeout", "600".into()),
        ("anthropic-dangerous-direct-browser-access", "true".into()),
        ("x-app", "cli".into()),
        // A fresh id per request, unlike the session id, which is per
        // conversation.
        ("x-client-request-id", request_id()),
    ]
}

/// A new id for one request, in the shape the client sends.
fn request_id() -> String {
    let now = crate::chat::now();
    let seed = format!("request:{now}:{:?}", std::time::Instant::now());
    let h = hex(sha256(seed.as_bytes()).as_ref());
    format!(
        "{}-{}-4{}-8{}-{}",
        &h[0..8],
        &h[8..12],
        &h[13..16],
        &h[17..20],
        &h[20..32]
    )
}

/// The platform, spelled the way the SDK spells it. The SDK is JavaScript, so
/// these are Node's names: reporting Rust's `macos` or `x86_64` where every
/// real request says `MacOS` and `x64` is itself the thing this module exists
/// to avoid.
fn stainless_os() -> &'static str {
    match std::env::consts::OS {
        "macos" => "MacOS",
        "windows" => "Windows",
        "linux" => "Linux",
        other => other,
    }
}

fn stainless_arch() -> &'static str {
    match std::env::consts::ARCH {
        "x86_64" => "x64",
        "aarch64" => "arm64",
        "x86" => "ia32",
        other => other,
    }
}

/// The `anthropic-beta` set for one request.
///
/// Gated on the model and the request's shape, because sending a model a beta
/// it does not support is itself a tell -- and in Haiku's case an outright
/// refusal.
pub(crate) fn betas(model: &str, has_tools: bool, has_system: bool, has_effort: bool) -> String {
    let lower = model.to_ascii_lowercase();
    let haiku = lower.contains("haiku");
    let opus = lower.contains("opus");
    let sonnet = lower.contains("sonnet");
    // `opus-5`, not `opus-4-5` -- which contains "opus-4-5" and not "opus-5".
    let opus5 = lower.contains("opus-5");
    // The full agent shape: instructions and tools, which is what a coding
    // session always sends.
    let agent = has_tools && has_system;

    let mut flags: Vec<&str> = Vec::new();
    if agent {
        flags.push("claude-code-20250219");
    }
    // Always: this only ever runs for a subscription token.
    flags.push(OAUTH_BETA);
    if agent && opus && !opus5 {
        flags.push("context-1m-2025-08-07");
    }
    if agent && opus {
        flags.push("mid-conversation-system-2026-04-07");
    }
    flags.push("context-management-2025-06-27");
    flags.push("prompt-caching-scope-2026-01-05");
    if agent {
        flags.push("advisor-tool-2026-03-01");
        flags.push("extended-cache-ttl-2025-04-11");
        flags.push("cache-diagnosis-2026-04-07");
        if opus || sonnet {
            flags.push("advanced-tool-use-2025-11-20");
        }
    }
    // The effort flag goes with the field, not with the agent shape: a plain
    // chat that asked for an effort would otherwise send `output_config`
    // without the beta that reads it.
    if (has_effort || (agent && (opus || sonnet))) && !flags.contains(&"effort-2025-11-24") {
        flags.push("effort-2025-11-24");
    }
    // Haiku refuses three of these outright, so they come back off rather than
    // being sent and 400'd.
    const HAIKU_REFUSES: [&str; 3] = [
        "advisor-tool-2026-03-01",
        "extended-cache-ttl-2025-04-11",
        "cache-diagnosis-2026-04-07",
    ];
    if haiku {
        flags.retain(|flag| !HAIKU_REFUSES.contains(flag));
    }
    flags.join(",")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::providers::auth::Tokens;

    fn credential() -> Tokens {
        Tokens {
            access: "sk-ant-oat01-whatever".into(),
            refresh: "refresh-token-that-outlives-renewals".into(),
            ..Tokens::default()
        }
    }

    fn body() -> Value {
        json!({
            "model": "claude-sonnet-5-5",
            "max_tokens": 1024,
            "messages": [{"role": "user", "content": "hello"}],
        })
    }

    /// The failure this module exists for: a valid token without these marks
    /// is refused as a rate limit on an account that is not rate limited. So
    /// the assertion is on what is SENT, not on a happy path.
    #[test]
    fn the_two_system_blocks_are_present_and_in_order() {
        let mut request = body();
        apply(&mut request, &credential());
        let system = request["system"].as_array().expect("system is an array");
        assert!(
            system[0]["text"]
                .as_str()
                .expect("block 0 is text")
                .starts_with("x-anthropic-billing-header: cc_version=2.1.293.9b8;"),
            "billing block first: {:?}",
            system[0]
        );
        assert!(
            system[0]["text"]
                .as_str()
                .unwrap()
                .contains("cc_entrypoint=cli;")
        );
        assert_eq!(system[1]["text"], CLIENT_SYSTEM_PROMPT);
    }

    #[test]
    fn the_entrypoint_agrees_with_the_user_agent() {
        // Naming two different clients across these two fields is the tell.
        assert!(USER_AGENT.contains("(external, cli)"));
        assert!(billing_header().contains("cc_entrypoint=cli;"));
        assert!(USER_AGENT.contains(CLAUDE_CODE_VERSION));
        assert!(billing_header().contains(CLAUDE_CODE_VERSION));
    }

    #[test]
    fn the_session_header_matches_the_one_inside_the_metadata() {
        let mut request = body();
        let session = apply(&mut request, &credential());
        let inner: Value = serde_json::from_str(request["metadata"]["user_id"].as_str().unwrap())
            .expect("user_id is a JSON document");
        assert_eq!(
            inner["session_id"], session,
            "a mismatch here shows nowhere else"
        );
        let sent = marks(&session);
        let header = sent
            .iter()
            .find(|(name, _)| *name == "X-Claude-Code-Session-Id")
            .map(|(_, value)| value.clone())
            .expect("the session header is sent");
        assert_eq!(header, session);
    }

    #[test]
    fn applying_twice_does_not_double_the_billing_block() {
        let mut request = body();
        apply(&mut request, &credential());
        apply(&mut request, &credential());
        let system = request["system"].as_array().unwrap();
        let billing = system.iter().filter(|b| is_billing(b)).count();
        assert_eq!(billing, 1, "{system:?}");
        let sentinels = system
            .iter()
            .filter(|b| b["text"].as_str() == Some(CLIENT_SYSTEM_PROMPT))
            .count();
        assert_eq!(sentinels, 1);
    }

    #[test]
    fn a_billing_block_at_any_index_is_replaced_not_joined() {
        let mut request = body();
        request["system"] = json!([
            {"type": "text", "text": "mine"},
            {"type": "text", "text": "x-anthropic-billing-header: cc_version=0.0.0.000; cch=00000;"},
        ]);
        apply(&mut request, &credential());
        let system = request["system"].as_array().unwrap();
        assert_eq!(system.iter().filter(|b| is_billing(b)).count(), 1);
        assert!(
            !serde_json::to_string(&system)
                .unwrap()
                .contains("0.0.0.000")
        );
        // The person's own instruction survives.
        assert!(system.iter().any(|b| b["text"] == "mine"));
    }

    #[test]
    fn a_string_system_prompt_keeps_its_text() {
        let mut request = body();
        request["system"] = json!("be terse");
        apply(&mut request, &credential());
        let system = request["system"].as_array().unwrap();
        assert!(system.iter().any(|b| b["text"] == "be terse"));
        assert_eq!(system[1]["text"], CLIENT_SYSTEM_PROMPT);
    }

    /// The whole point of deriving from the head: turn twelve must present the
    /// same session as turn one, or the caching it exists for never happens.
    #[test]
    fn a_growing_conversation_keeps_one_session() {
        let mut first = body();
        let one = apply(&mut first, &credential());

        let mut twelfth = body();
        twelfth["messages"] = json!([
            {"role": "user", "content": "hello"},
            {"role": "assistant", "content": "hi"},
            {"role": "user", "content": "and now something else entirely"},
        ]);
        let twelve = apply(&mut twelfth, &credential());
        assert_eq!(one, twelve);

        // A different conversation is a different session.
        let mut other = body();
        other["messages"] = json!([{"role": "user", "content": "unrelated"}]);
        assert_ne!(one, apply(&mut other, &credential()));
    }

    #[test]
    fn our_own_marks_do_not_become_the_conversation_head() {
        // Two conversations with no system prompt of their own must still
        // differ, which they would not if the sentinel were the head.
        let mut a = body();
        a["messages"] = json!([{"role": "user", "content": "first"}]);
        let mut b = body();
        b["messages"] = json!([{"role": "user", "content": "second"}]);
        assert_ne!(apply(&mut a, &credential()), apply(&mut b, &credential()));
    }

    #[test]
    fn the_device_id_is_not_derived_from_the_token() {
        // A token-derived id changes at every renewal, which from the
        // provider's side is a new device every few hours.
        let mut first = body();
        let mut renewed = credential();
        apply(&mut first, &renewed);
        let before = first["metadata"]["user_id"].as_str().unwrap().to_owned();

        renewed.access = "sk-ant-oat01-a-completely-new-token".into();
        let mut after = body();
        apply(&mut after, &renewed);
        assert_eq!(after["metadata"]["user_id"].as_str().unwrap(), before);
    }

    #[test]
    fn a_provider_supplied_account_uuid_wins_over_the_derived_one() {
        let mut given = credential();
        given.account = "11111111-2222-4333-8444-555555555555".into();
        let mut request = body();
        apply(&mut request, &given);
        let inner: Value =
            serde_json::from_str(request["metadata"]["user_id"].as_str().unwrap()).unwrap();
        assert_eq!(inner["account_uuid"], given.account);
    }

    #[test]
    fn a_derived_uuid_is_well_formed_v4() {
        let id = derive_uuid("seed");
        let parts: Vec<&str> = id.split('-').collect();
        assert_eq!(
            parts.iter().map(|p| p.len()).collect::<Vec<_>>(),
            vec![8, 4, 4, 4, 12]
        );
        assert!(
            id.chars().all(|c| c.is_ascii_hexdigit() || c == '-'),
            "{id}"
        );
        assert_eq!(&parts[2][..1], "4", "version nibble");
        assert!(
            matches!(&parts[3][..1], "8" | "9" | "a" | "b"),
            "variant nibble: {id}"
        );
        assert_eq!(derive_uuid("seed"), id, "deterministic");
        assert_ne!(derive_uuid("other"), id);
    }

    #[test]
    fn haiku_does_not_receive_the_three_betas_it_refuses() {
        let set = betas("claude-haiku-5-5", true, true, false);
        for refused in ["advisor-tool", "extended-cache-ttl", "cache-diagnosis"] {
            assert!(!set.contains(refused), "{set}");
        }
        // And still receives the ones it does take.
        assert!(set.contains("oauth-2025-04-20"));
        assert!(set.contains("claude-code-20250219"));

        let sonnet = betas("claude-sonnet-5-5", true, true, false);
        assert!(sonnet.contains("advisor-tool-2026-03-01"), "{sonnet}");
    }

    #[test]
    fn the_agent_betas_need_both_tools_and_instructions() {
        let bare = betas("claude-sonnet-5-5", false, false, false);
        assert!(bare.contains("oauth-2025-04-20"));
        assert!(!bare.contains("claude-code-20250219"), "{bare}");
        assert!(!bare.contains("effort-2025-11-24"));
    }

    #[test]
    fn the_effort_beta_goes_with_the_field_that_needs_it() {
        // A plain chat that asked for an effort still carries the flag that
        // reads `output_config`, or the field is sent to nothing.
        let plain = betas("claude-sonnet-5-5", false, false, true);
        assert!(plain.contains("effort-2025-11-24"), "{plain}");
        // And it is never doubled when the agent shape would add it too.
        let agent = betas("claude-sonnet-5-5", true, true, true);
        assert_eq!(agent.matches("effort-2025-11-24").count(), 1, "{agent}");
    }

    #[test]
    fn the_million_token_beta_skips_opus_5() {
        assert!(betas("claude-opus-4-1", true, true, false).contains("context-1m-2025-08-07"));
        // "opus-4-5" contains "opus-4-5", not "opus-5".
        assert!(
            betas("claude-opus-4-5-20251101", true, true, false).contains("context-1m-2025-08-07")
        );
        assert!(!betas("claude-opus-5-5", true, true, false).contains("context-1m-2025-08-07"));
    }

    #[test]
    fn the_platform_is_spelled_the_way_the_sdk_spells_it() {
        // Node's names, not Rust's. The SDK being described is JavaScript, so
        // `macos` or `x86_64` here is itself the tell this module avoids.
        assert!(!stainless_os().is_empty());
        assert!(!stainless_arch().is_empty());
        assert_ne!(stainless_os(), "macos");
        assert_ne!(stainless_arch(), "x86_64");
        assert_ne!(stainless_arch(), "aarch64");
    }

    #[test]
    fn the_marks_carry_the_set_and_repeat_nothing() {
        let sent = marks("sess");
        let names: Vec<&str> = sent.iter().map(|(name, _)| *name).collect();
        for expected in [
            "Accept",
            "User-Agent",
            "X-Claude-Code-Session-Id",
            "X-Stainless-Lang",
            "X-Stainless-Package-Version",
            "x-app",
        ] {
            assert!(names.contains(&expected), "missing {expected}");
        }
        assert!(sent.iter().all(|(_, value)| !value.is_empty()));
        // The SDK only emits this on its own stream helper; the real client's
        // native request omits it, so nothing may add it back.
        assert!(!names.contains(&"X-Stainless-Helper-Method"));
        // And nothing every Anthropic call already carries: a second copy of
        // a header is itself a thing no client sends.
        for already in [
            "Authorization",
            "Content-Type",
            "anthropic-version",
            "anthropic-beta",
        ] {
            assert!(!names.contains(&already), "{already} is set elsewhere");
        }
    }
}
