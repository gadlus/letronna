//! Why a model call failed, in a form code can act on.
//!
//! A failure used to be a sentence for the user and nothing else, so nothing
//! could tell a rate limit, which passes, from a bad key, which does not.
//! Each one now carries a `FailureKind` as well. The sentence is kept apart
//! and is word for word what was shown before, so the window and the terminal
//! say the same thing whatever is later done with the kind.
//!
//! Providers disagree about how to say what went wrong, so `classify` asks in
//! the order the answers can be trusted: the HTTP status where it settles the
//! matter, then the provider's own `error.code` or `error.type`, then an
//! ordered table of phrases. An error that arrives inside a stream that began
//! with 200 goes through the same steps, with the numeric `error.code`
//! OpenRouter puts in its envelope standing in for the status it could not
//! send.

use reqwest::header::{HeaderMap, RETRY_AFTER};
use serde_json::Value;
use std::{fmt, time::Duration};

/// What went wrong with a model call, as far as can be told.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FailureKind {
    /// The key is missing, wrong, or not allowed to do this.
    Auth,
    /// The account is out of credit, which waiting will not change.
    Quota,
    /// Too many requests for now; the same one goes through after a wait.
    RateLimit,
    /// The provider is too busy to take any request.
    Overloaded,
    /// The conversation is longer than the model can read at once.
    ContextTooLong,
    /// The provider turned the request down as it was written.
    BadRequest,
    /// The model does not exist there.
    NotFound,
    /// Something broke at the provider's end.
    Server,
    /// The provider could not be reached, or the connection ended before the reply did.
    Network,
    /// The provider answered with nothing that could be used.
    BadReply,
    /// The connection settings cannot work as they are.
    Settings,
    /// Nobody is waiting for the reply any more.
    Stopped,
    /// Nothing said what went wrong.
    Unknown,
}

/// A failed model call: its kind, and the words the user is shown for it.
#[derive(Debug)]
pub struct Failure {
    pub kind: FailureKind,
    /// How long the provider asked to be left alone, when it said.
    pub retry_after: Option<Duration>,
    message: String,
}

impl Failure {
    /// A failure whose kind is known without a provider's reply to read.
    pub fn new(kind: FailureKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            retry_after: None,
            message: message.into(),
        }
    }
}

impl fmt::Display for Failure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

/// Read a provider's error reply, given its HTTP status or `None` when the
/// error came inside a reply that began as a success.
pub fn classify(status: Option<u16>, headers: &HeaderMap, body: &Value) -> Failure {
    let error = body.get("error");
    let code = status.or_else(|| u16::try_from(error?.get("code")?.as_u64()?).ok());
    // The whole envelope, so the copy of the upstream reply OpenRouter keeps
    // in `metadata.raw` is read as well: its own message only says "Provider
    // returned error".
    let text = error.unwrap_or(body).to_string().to_lowercase();
    let kind = code
        .and_then(|code| by_status(code, error))
        .or_else(|| by_code(error))
        .or_else(|| {
            PHRASES
                .iter()
                .find(|(phrase, _)| text.contains(phrase))
                .map(|(_, kind)| *kind)
        })
        .unwrap_or(match code {
            // A 404 that names no missing model is more often a wrong base
            // URL, which no other model would mend.
            Some(404) => FailureKind::Settings,
            Some(400..=499) => FailureKind::BadRequest,
            _ => FailureKind::Unknown,
        });
    Failure {
        kind,
        retry_after: retry_after(headers),
        message: words(status, body),
    }
}

/// The kind a status settles on its own, which 400 and 404 do not: an overflow
/// and a bad parameter both come back as 400, a missing model and a wrong base
/// URL both as 404.
fn by_status(status: u16, error: Option<&Value>) -> Option<FailureKind> {
    Some(match status {
        401 | 403 => FailureKind::Auth,
        402 => FailureKind::Quota,
        408 => FailureKind::Network,
        413 => FailureKind::ContextTooLong,
        // OpenAI answers an empty balance with 429 too. Only its code tells
        // the two apart, since Gemini words a per-minute limit as an exceeded
        // quota as well.
        429 if by_code(error) == Some(FailureKind::Quota) => FailureKind::Quota,
        429 => FailureKind::RateLimit,
        529 => FailureKind::Overloaded,
        500..=599 => FailureKind::Server,
        _ => return None,
    })
}

/// Names providers give their errors in `error.code` or `error.type`.
const CODES: &[(&str, FailureKind)] = &[
    // `invalid_request_error` is left out: OpenAI, Anthropic and older
    // llama.cpp give it to an overflow too, which only the words tell apart.
    ("invalid_api_key", FailureKind::Auth),
    ("authentication_error", FailureKind::Auth),
    ("permission_error", FailureKind::Auth),
    ("insufficient_quota", FailureKind::Quota),
    ("rate_limit_exceeded", FailureKind::RateLimit),
    ("rate_limit_error", FailureKind::RateLimit),
    ("overloaded_error", FailureKind::Overloaded),
    ("context_length_exceeded", FailureKind::ContextTooLong),
    ("exceed_context_size_error", FailureKind::ContextTooLong),
    ("request_too_large", FailureKind::ContextTooLong),
    ("model_not_found", FailureKind::NotFound),
    ("not_found_error", FailureKind::NotFound),
    ("server_error", FailureKind::Server),
    ("unavailable_error", FailureKind::Server),
];

/// The kind named by the envelope's `code`, or else by its `type`.
fn by_code(error: Option<&Value>) -> Option<FailureKind> {
    let error = error?;
    ["code", "type"]
        .into_iter()
        .filter_map(|field| error.get(field)?.as_str())
        .find_map(|name| {
            CODES
                .iter()
                .find(|(code, _)| *code == name)
                .map(|(_, kind)| *kind)
        })
}

/// Words that name the kind when neither the status nor a code did, in lowercase.
const PHRASES: &[(&str, FailureKind)] = &[
    // In order: where a reply says two of these, the first is the one to act
    // on -- an account out of credit is not mended by a new key.
    ("prompt is too long", FailureKind::ContextTooLong),
    ("context length", FailureKind::ContextTooLong),
    ("context size", FailureKind::ContextTooLong),
    ("credit balance", FailureKind::Quota),
    ("rate limit", FailureKind::RateLimit),
    ("overloaded", FailureKind::Overloaded),
    ("api key", FailureKind::Auth),
    ("not exist", FailureKind::NotFound),
    ("not found", FailureKind::NotFound),
];

/// The wait `Retry-After` asks for, from now.
fn retry_after(headers: &HeaderMap) -> Option<Duration> {
    let value = headers.get(RETRY_AFTER)?.to_str().ok()?;
    wait_asked(value, chrono::Utc::now())
}

/// The wait a `Retry-After` value asks for at `now`, given in seconds or as
/// the date to come back; a date already gone asks for none.
fn wait_asked(value: &str, now: chrono::DateTime<chrono::Utc>) -> Option<Duration> {
    let value = value.trim();
    if let Ok(seconds) = value.parse::<f64>() {
        return Duration::try_from_secs_f64(seconds).ok();
    }
    // HTTP dates are RFC 2822's, always in GMT.
    let date = chrono::DateTime::parse_from_rfc2822(value).ok()?;
    date.signed_duration_since(now).to_std().ok()
}

/// What the user is told, word for word as before: the provider's own
/// complaint when it sent one, or else what its status means.
fn words(status: Option<u16>, body: &Value) -> String {
    if let Err(message) = crate::chat::unwrap_response(body) {
        return message;
    }
    let Some(code) = status else {
        return "The provider could not finish this response.".to_owned();
    };
    if reqwest::StatusCode::from_u16(code).is_ok_and(crate::chat::transient) {
        format!(
            "The gateway is having trouble right now (HTTP {code}). This is at                      the provider's end, not your settings. Try again in a moment, or                      switch gateway in Settings."
        )
    } else {
        format!("Provider returned HTTP {code}. Check connection settings and quota.")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The kind of a reply that came with `status` and `body`.
    fn kind(status: Option<u16>, body: &str) -> FailureKind {
        let body: Value = serde_json::from_str(body).unwrap();
        classify(status, &HeaderMap::new(), &body).kind
    }

    #[test]
    fn openai_errors_each_get_their_own_kind() {
        let cases = [
            (
                401,
                r#"{"error":{"message":"Incorrect API key provided: sk-proj-****abcd. You can find your API key at https://platform.openai.com/account/api-keys.","type":"invalid_request_error","param":null,"code":"invalid_api_key"}}"#,
                FailureKind::Auth,
            ),
            (
                429,
                r#"{"error":{"message":"Rate limit reached for gpt-4o in organization org-abc on tokens per min (TPM): Limit 30000, Used 29000, Requested 2000. Please try again in 2s. Visit https://platform.openai.com/account/rate-limits to learn more.","type":"tokens","param":null,"code":"rate_limit_exceeded"}}"#,
                FailureKind::RateLimit,
            ),
            (
                429,
                r#"{"error":{"message":"You exceeded your current quota, please check your plan and billing details. For more information on this error, read the docs: https://platform.openai.com/docs/guides/error-codes/api-errors.","type":"insufficient_quota","param":null,"code":"insufficient_quota"}}"#,
                FailureKind::Quota,
            ),
            (
                400,
                r#"{"error":{"message":"This model's maximum context length is 128000 tokens. However, your messages resulted in 130532 tokens. Please reduce the length of the messages.","type":"invalid_request_error","param":"messages","code":"context_length_exceeded"}}"#,
                FailureKind::ContextTooLong,
            ),
            (
                404,
                r#"{"error":{"message":"The model `gpt-9` does not exist or you do not have access to it.","type":"invalid_request_error","param":null,"code":"model_not_found"}}"#,
                FailureKind::NotFound,
            ),
            // A base URL missing its /v1, which no other model would mend.
            (
                404,
                r#"{"error":{"message":"Invalid URL (POST /chat/completions)","type":"invalid_request_error","param":null,"code":null}}"#,
                FailureKind::Settings,
            ),
            (
                400,
                r#"{"error":{"message":"Unsupported parameter: 'max_tokens' is not supported with this model. Use 'max_completion_tokens' instead.","type":"invalid_request_error","param":"max_tokens","code":"unsupported_parameter"}}"#,
                FailureKind::BadRequest,
            ),
            (
                500,
                r#"{"error":{"message":"The server had an error while processing your request. Sorry about that!","type":"server_error","param":null,"code":null}}"#,
                FailureKind::Server,
            ),
        ];
        for (status, body, expected) in cases {
            assert_eq!(kind(Some(status), body), expected, "{body}");
        }
    }

    #[test]
    fn anthropic_errors_through_openrouter_are_read_from_the_upstream_reply() {
        let cases = [
            (
                400,
                r#"{"error":{"message":"Provider returned error","code":400,"metadata":{"raw":"{\"type\":\"error\",\"error\":{\"type\":\"invalid_request_error\",\"message\":\"prompt is too long: 208437 tokens > 200000 maximum\"}}","provider_name":"Anthropic"}}}"#,
                FailureKind::ContextTooLong,
            ),
            (
                400,
                r#"{"error":{"message":"Provider returned error","code":400,"metadata":{"raw":"{\"type\":\"error\",\"error\":{\"type\":\"invalid_request_error\",\"message\":\"Your credit balance is too low to access the Anthropic API. Please go to Plans & Billing to upgrade or purchase credits.\"}}","provider_name":"Anthropic"}}}"#,
                FailureKind::Quota,
            ),
            (
                429,
                r#"{"error":{"message":"Provider returned error","code":429,"metadata":{"raw":"{\"type\":\"error\",\"error\":{\"type\":\"rate_limit_error\",\"message\":\"This request would exceed the rate limit for your organization of 40,000 input tokens per minute.\"}}","provider_name":"Anthropic"}}}"#,
                FailureKind::RateLimit,
            ),
            (
                529,
                r#"{"error":{"message":"Provider returned error","code":529,"metadata":{"raw":"{\"type\":\"error\",\"error\":{\"type\":\"overloaded_error\",\"message\":\"Overloaded\"}}","provider_name":"Anthropic"}}}"#,
                FailureKind::Overloaded,
            ),
            // OpenRouter's own refusals, before any provider is asked.
            (
                400,
                r#"{"error":{"message":"This endpoint's maximum context length is 200000 tokens. However, you requested about 213000 tokens (204808 of text input, 8192 in the output). Please reduce the length of either one, or use the \"middle-out\" transform to compress your prompt automatically.","code":400,"metadata":{"provider_name":null}}}"#,
                FailureKind::ContextTooLong,
            ),
            (
                402,
                r#"{"error":{"message":"This request requires more credits, or fewer max_tokens. You requested up to 8192 tokens, but can only afford 1715. To increase, visit https://openrouter.ai/settings/credits and upgrade to a paid account","code":402,"metadata":{"provider_name":null}}}"#,
                FailureKind::Quota,
            ),
        ];
        for (status, body, expected) in cases {
            assert_eq!(kind(Some(status), body), expected, "{body}");
        }
    }

    #[test]
    fn ollama_errors_each_get_their_own_kind() {
        assert_eq!(
            kind(
                Some(404),
                r#"{"error":{"message":"model \"llama3.2\" not found, try pulling it first","type":"api_error","param":null,"code":null}}"#
            ),
            FailureKind::NotFound
        );
        assert_eq!(
            kind(
                Some(500),
                r#"{"error":{"message":"llama runner process has terminated: exit status 2","type":"api_error","param":null,"code":null}}"#
            ),
            FailureKind::Server
        );
    }

    #[test]
    fn llama_cpp_errors_each_get_their_own_kind() {
        let cases = [
            (
                400,
                r#"{"error":{"code":400,"message":"the request exceeds the available context size, try increasing it","type":"exceed_context_size_error","n_prompt_tokens":4589,"n_ctx":4096}}"#,
                FailureKind::ContextTooLong,
            ),
            // Before the overflow had a type of its own.
            (
                400,
                r#"{"error":{"code":400,"message":"the request exceeds the available context size. try increasing the context size or enable context shift","type":"invalid_request_error"}}"#,
                FailureKind::ContextTooLong,
            ),
            (
                503,
                r#"{"error":{"code":503,"message":"Loading model","type":"unavailable_error"}}"#,
                FailureKind::Server,
            ),
            (
                401,
                r#"{"error":{"code":401,"message":"Invalid API Key","type":"authentication_error"}}"#,
                FailureKind::Auth,
            ),
        ];
        for (status, body, expected) in cases {
            assert_eq!(kind(Some(status), body), expected, "{body}");
        }
    }

    #[test]
    fn an_error_inside_a_stream_takes_its_status_from_the_envelope() {
        // OpenRouter, once the stream has begun, where only the number says
        // what went wrong.
        let cases = [
            (401, FailureKind::Auth),
            (402, FailureKind::Quota),
            (404, FailureKind::Settings),
            (429, FailureKind::RateLimit),
            (529, FailureKind::Overloaded),
            (502, FailureKind::Server),
            (400, FailureKind::BadRequest),
        ];
        for (code, expected) in cases {
            let body = format!(
                r#"{{"error":{{"message":"Provider returned error","code":{code},"metadata":{{"provider_name":"Anthropic"}}}}}}"#
            );
            assert_eq!(kind(None, &body), expected, "{body}");
        }
        // Then a named code.
        assert_eq!(
            kind(
                None,
                r#"{"id":"gen-1759000000-abc","object":"chat.completion.chunk","model":"anthropic/claude-sonnet-4","provider":"Anthropic","error":{"code":"server_error","message":"Provider disconnected unexpectedly"},"choices":[{"index":0,"delta":{"content":""},"finish_reason":"error"}]}"#
            ),
            FailureKind::Server
        );
        // Anthropic's own event, where only the type names it.
        assert_eq!(
            kind(
                None,
                r#"{"type":"error","error":{"type":"overloaded_error","message":"Overloaded"}}"#
            ),
            FailureKind::Overloaded
        );
    }

    #[test]
    fn the_phrases_decide_what_the_status_and_code_left_open() {
        let cases = [
            // Gemini says a bad key with 400.
            (
                Some(400),
                r#"{"error":{"code":400,"message":"API key not valid. Please pass a valid API key.","status":"INVALID_ARGUMENT"}}"#,
                FailureKind::Auth,
            ),
            // DeepSeek says a missing model with 400.
            (
                Some(400),
                r#"{"error":{"message":"Model Not Exist","type":"invalid_request_error","param":null,"code":"invalid_request_error"}}"#,
                FailureKind::NotFound,
            ),
            // Envelopes with nothing but words, as a gateway may pass them on
            // inside a stream.
            (
                None,
                r#"{"error":{"message":"Rate limit reached for gpt-4o in organization org-abc on requests per min (RPM): Limit 500, Used 500, Requested 1."}}"#,
                FailureKind::RateLimit,
            ),
            (
                None,
                r#"{"error":{"message":"Overloaded"}}"#,
                FailureKind::Overloaded,
            ),
            (
                None,
                r#"{"error":{"message":"Something odd happened."}}"#,
                FailureKind::Unknown,
            ),
        ];
        for (status, body, expected) in cases {
            assert_eq!(kind(status, body), expected, "{body}");
        }
    }

    #[test]
    fn only_an_empty_balance_makes_a_429_a_quota() {
        // Gemini's per-minute limit talks about quota but passes with a wait.
        assert_eq!(
            kind(
                Some(429),
                r#"{"error":{"code":429,"message":"You exceeded your current quota, please check your plan and billing details. For more information on this error, head to: https://ai.google.dev/gemini-api/docs/rate-limits.","status":"RESOURCE_EXHAUSTED"}}"#
            ),
            FailureKind::RateLimit
        );
    }

    #[test]
    fn a_reply_with_nothing_to_read_is_judged_by_its_status() {
        let bare = |status| classify(Some(status), &HeaderMap::new(), &Value::Null).kind;
        assert_eq!(bare(401), FailureKind::Auth);
        // Ollama's plain "404 page not found" at a wrong base URL comes to this.
        assert_eq!(bare(404), FailureKind::Settings);
        assert_eq!(bare(413), FailureKind::ContextTooLong);
        assert_eq!(bare(429), FailureKind::RateLimit);
        assert_eq!(bare(418), FailureKind::BadRequest);
        assert_eq!(bare(502), FailureKind::Server);
        assert_eq!(bare(529), FailureKind::Overloaded);
        assert_eq!(bare(301), FailureKind::Unknown);
    }

    #[test]
    fn the_words_are_the_ones_shown_before() {
        let said = |status, body: &str| {
            let body: Value = serde_json::from_str(body).unwrap_or_default();
            classify(status, &HeaderMap::new(), &body).to_string()
        };
        // The provider's complaint, as it wrote it.
        assert_eq!(
            said(Some(429), r#"{"error":{"message":"Slow down."}}"#),
            "Slow down."
        );
        // Explained where it would mislead.
        assert!(
            said(
                Some(400),
                r#"{"error":{"message":"Function call is missing a thought_signature in functionCall parts."}}"#
            )
            .contains("/model")
        );
        // A page that is not JSON leaves only the status to go on.
        assert!(
            said(Some(503), "<html>Service Unavailable</html>")
                .starts_with("The gateway is having trouble right now (HTTP 503).")
        );
        assert_eq!(
            said(Some(418), ""),
            "Provider returned HTTP 418. Check connection settings and quota."
        );
    }

    #[test]
    fn a_wait_asked_for_in_seconds_is_kept() {
        let wait = |value: &str| {
            let mut headers = HeaderMap::new();
            headers.insert(RETRY_AFTER, value.parse().unwrap());
            classify(Some(429), &headers, &Value::Null).retry_after
        };
        assert_eq!(wait("7"), Some(Duration::from_secs(7)));
        assert_eq!(wait("1.5"), Some(Duration::from_millis(1500)));
        assert_eq!(wait("-3"), None);
        assert_eq!(
            classify(Some(429), &HeaderMap::new(), &Value::Null).retry_after,
            None
        );
    }

    #[test]
    fn a_wait_asked_for_as_a_date_counts_from_now() {
        let now = chrono::DateTime::parse_from_rfc2822("Wed, 21 Oct 2026 07:27:30 GMT")
            .unwrap()
            .to_utc();
        let wait = |value| wait_asked(value, now);
        assert_eq!(
            wait("Wed, 21 Oct 2026 07:28:00 GMT"),
            Some(Duration::from_secs(30))
        );
        assert_eq!(wait("Wed, 21 Oct 2026 07:27:30 GMT"), Some(Duration::ZERO));
        assert_eq!(wait("Wed, 21 Oct 2026 07:27:00 GMT"), None);
        assert_eq!(wait(" 12 "), Some(Duration::from_secs(12)));
        assert_eq!(wait("soon"), None);
    }
}
