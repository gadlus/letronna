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
//! send. Where it has none, a status its words name, as "(HTTP 502)" does, is
//! read after the provider's code and before the phrases. `chat` puts such an
//! error that names nothing, and whose code and type do not say the request
//! was refused, down to the provider, which had taken the request.
//!
//! A failed turn is kept in its session as a message of its own under
//! `ROLE`, so it is there after a restart and goes wherever the session
//! goes. The model never reads one: `chat` leaves it out of each request and
//! each summary. With it is kept what the window needs to say the cause in
//! a few words and offer at most two ways out, the likely one first.

use crate::chat::Message;
use reqwest::header::{HeaderMap, RETRY_AFTER};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{fmt, time::Duration};

/// What went wrong with a model call, as far as can be told.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
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
    // A kind a newer version kept reads as this, rather than losing the
    // message it is on.
    #[serde(other)]
    Unknown,
}

/// A failed model call: its kind, and the words the user is shown for it.
#[derive(Debug)]
pub struct Failure {
    pub kind: FailureKind,
    /// How long the provider asked to be left alone, when it said.
    pub retry_after: Option<Duration>,
    /// The limits an overflow named, when the failure is one.
    pub overflow: Option<Overflow>,
    /// Part of the reply was already passed on, so it cannot be asked for
    /// again without showing a second one after it.
    pub stands: bool,
    /// The reply reached its token limit before it wrote anything, all of
    /// it spent thinking, so a longer limit could mend it.
    pub out_of_room: bool,
    /// The gateway the failed call went to, once `fallback::run` has said:
    /// the one that failed, which after a switch is not the session's.
    pub gateway: Option<&'static str>,
    /// A server error that waiting will not mend, such as a gateway whose own
    /// upstream account has run out of credit: not tried again, though
    /// another model may answer it.
    pub lasting: bool,
    message: String,
}

/// What an overflow error said about the limits it ran into, in tokens;
/// `None` is not said.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Overflow {
    /// The most one request may hold, prompt and reply together.
    pub context: Option<u64>,
    /// What the prompt came to, as the provider counted it.
    pub input: Option<u64>,
    /// The most one reply may run to.
    pub output: Option<u64>,
}

impl Failure {
    /// A failure whose kind is known without a provider's reply to read.
    pub fn new(kind: FailureKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            retry_after: None,
            overflow: None,
            stands: false,
            out_of_room: false,
            gateway: None,
            lasting: false,
            message: message.into(),
        }
    }

    /// Whether the call went to the Letronna account, whose plan and credit
    /// decide what it serves, unlike a key of the user's own.
    pub fn hosted(&self) -> bool {
        self.gateway == Some(crate::gateway::LETRONNA)
    }
}

impl fmt::Display for Failure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

/// The role a failed turn is kept under in its session.
pub const ROLE: &str = "failure";

/// The longest wait worth offering to come back after. A `Retry-After` of
/// years has no time of day to show and would tick on the row for as long.
const LONGEST_KEPT_WAIT: Duration = Duration::from_secs(24 * 60 * 60);

/// What a session keeps of a failed turn beside its words.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Kept {
    pub kind: FailureKind,
    /// When the provider said to come back, in seconds since 1970.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub until: Option<u64>,
    /// The call went to the Letronna account, whose credit and sign-in can
    /// be mended from here, unlike another provider's.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub hosted: bool,
    /// A subscription is the way out of this hosted failure, as `upgrade`
    /// tells, rather than credit or a sign-in.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub upgrade: bool,
    /// A gateway besides Letronna was set up, so a model there carries on
    /// at once while the account waits to be paid for.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub elsewhere: bool,
}

/// A way out of a failed turn, as a button offers it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fix {
    Retry,
    /// Retry once the provider's wait is over, at this second since 1970.
    RetryAt(u64),
    AddCredit,
    /// Subscribe on the website, for a Free plan that cannot pay for the model.
    Upgrade,
    ChooseModel,
    SignIn,
    CopyDetails,
}

/// Whether a call to `gateway` that failed as `kind` is worth asking the
/// account service about again: the hosted pool turning it down for want of
/// credit or a plan, as when a perk's allowance for the day runs out or the
/// plan changed, after which what is open is not what was kept.
pub fn ask_account_again(gateway: Option<&str>, kind: FailureKind) -> bool {
    gateway == Some(crate::gateway::LETRONNA)
        && matches!(kind, FailureKind::Quota | FailureKind::Auth)
}

/// Whether a subscription is the way out of a hosted failure of `kind`
/// with `account` signed in here: a model the Free plan does not include,
/// or a Free plan with no credit left. Signed out, a sign-in comes first.
pub fn upgrade(kind: FailureKind, account: Option<&crate::auth::Account>) -> bool {
    account.is_some_and(|account| match kind {
        FailureKind::Auth => account.plan_name() == "Free",
        FailureKind::Quota => account.needs_upgrade(),
        _ => false,
    })
}

impl Failure {
    /// The failure as a message for its session, from a call to `model` at
    /// `now`, in seconds since 1970; `None` for a turn that was stopped,
    /// which is no failure.
    pub fn kept(&self, model: &str, hosted: bool, now: u64) -> Option<Message> {
        if self.kind == FailureKind::Stopped {
            return None;
        }
        // Only a wait that passes is worth coming back after, and one under
        // a second is over before anyone could press anything.
        let until = self
            .retry_after
            .filter(|wait| crate::retry::passes(self.kind) && wait.as_secs() > 0)
            .filter(|wait| *wait <= LONGEST_KEPT_WAIT)
            .map(|wait| now.saturating_add(wait.as_secs()));
        Some(Message {
            role: ROLE.into(),
            content: self.message.clone(),
            model: model.to_owned(),
            failed: Some(Box::new(Kept {
                kind: self.kind,
                until,
                hosted,
                upgrade: false,
                elsewhere: false,
            })),
            ..Default::default()
        })
    }
}

impl Kept {
    /// Take in what decides a hosted failure's way out: `account`, the one
    /// signed in here, and whether a gateway besides Letronna is set up.
    pub fn knowing(&mut self, account: Option<&crate::auth::Account>, elsewhere: bool) {
        self.upgrade = self.hosted && upgrade(self.kind, account);
        self.elsewhere = self.hosted && elsewhere;
    }

    /// The cause in a few plain words, with `clock` saying when `until` is.
    pub fn cause(&self, clock: impl Fn(u64) -> String) -> String {
        match self.kind {
            FailureKind::Auth if self.hosted && self.upgrade => "Needs a subscription".to_owned(),
            FailureKind::Auth if self.hosted => "Signed out".to_owned(),
            FailureKind::Auth => "The key was not accepted".to_owned(),
            FailureKind::Quota => "Out of credit".to_owned(),
            FailureKind::RateLimit => match self.until {
                Some(until) => format!("Rate limited until {}", clock(until)),
                None => "Rate limited".to_owned(),
            },
            FailureKind::Overloaded => "The provider is overloaded".to_owned(),
            FailureKind::ContextTooLong => "Context too long".to_owned(),
            FailureKind::BadRequest => "The provider turned the request down".to_owned(),
            FailureKind::NotFound => "Model not found".to_owned(),
            FailureKind::Server => "The provider had an error".to_owned(),
            FailureKind::Network => "Gateway unreachable".to_owned(),
            FailureKind::BadReply => "The reply could not be read".to_owned(),
            FailureKind::Settings => "The connection settings cannot work".to_owned(),
            FailureKind::Stopped => "Stopped".to_owned(),
            FailureKind::Unknown => "The reply failed".to_owned(),
        }
    }

    /// At most two ways out at `now`, the likely one first. A fix this
    /// window has no way to make, such as a key for another provider, is
    /// left to the words, and copying them is all that is offered.
    pub fn fixes(&self, now: u64) -> Vec<Fix> {
        let first = match self.kind {
            // Signed in on the Free plan, Settings has nothing to mend: only
            // a subscription does.
            FailureKind::Auth | FailureKind::Quota if self.hosted && self.upgrade => {
                Some(Fix::Upgrade)
            }
            FailureKind::Auth if self.hosted => Some(Fix::SignIn),
            FailureKind::Quota if self.hosted => Some(Fix::AddCredit),
            // A bigger window is the one way out the window can offer until
            // a session can be compacted on demand.
            FailureKind::NotFound | FailureKind::ContextTooLong => Some(Fix::ChooseModel),
            _ if crate::retry::passes(self.kind) => match self.until {
                Some(until) if until > now => Some(Fix::RetryAt(until)),
                _ => Some(Fix::Retry),
            },
            FailureKind::BadReply | FailureKind::Unknown => Some(Fix::Retry),
            _ => None,
        };
        // Paying takes a trip to the website; a model on a gateway already
        // set up answers now.
        let paying = matches!(first, Some(Fix::Upgrade | Fix::AddCredit));
        let second = if paying && self.elsewhere {
            Fix::ChooseModel
        } else {
            Fix::CopyDetails
        };
        first.into_iter().chain([second]).collect()
    }
}

/// What Copy details takes of the failure kept in `message`: enough to
/// report it by.
pub fn details(message: &Message) -> String {
    let kind = message
        .failed
        .as_ref()
        .map_or(FailureKind::Unknown, |kept| kept.kind);
    format!("{kind:?} from {}: {}", message.model, message.content)
}

/// A second since 1970 as the local clock shows it, such as 14:05.
pub fn clock(at: u64) -> String {
    i64::try_from(at)
        .ok()
        .and_then(|at| chrono::DateTime::from_timestamp(at, 0))
        .map(|at| at.with_timezone(&chrono::Local).format("%H:%M").to_string())
        .unwrap_or_default()
}

/// Read a provider's error reply, given its HTTP status or `None` when the
/// error came inside a reply that began as a success.
pub fn classify(status: Option<u16>, headers: &HeaderMap, body: &Value) -> Failure {
    classify_text(status, headers, body, "")
}

/// `classify` for a reply whose text as it came, `raw`, `body` was read
/// from. A reply that is no JSON, a gateway's error page say, is judged by
/// its status alone, and only its own words say what went wrong behind it.
pub fn classify_text(status: Option<u16>, headers: &HeaderMap, body: &Value, raw: &str) -> Failure {
    let error = body.get("error");
    // TGI gives the status of an error it sends inside a stream it has
    // already answered 200 under a field of its own.
    let field = |name: &str| u16::try_from(error?.get(name)?.as_u64()?).ok();
    let code = (status.or_else(|| field("code"))).or_else(|| field("http_status_code"));
    // The whole envelope, so the copy of the upstream reply OpenRouter keeps
    // in `metadata.raw` is read as well: its own message only says "Provider
    // returned error".
    let text = error.unwrap_or(body).to_string().to_lowercase();
    // A gateway that relays an upstream failure inside a stream it has
    // already answered 200 may give the upstream's status only in words, as
    // in "every lender we tried refused this request (HTTP 502)".
    let named = code.is_none().then(|| status_named(&text)).flatten();
    let settled = |code: u16| {
        by_status(code, error)
            // Ollama answers an overflow with a 500, as it does a crash, and
            // waiting would only send the same request again.
            .filter(|kind| {
                *kind != FailureKind::Server
                    || by_phrase(&text) != Some(FailureKind::ContextTooLong)
            })
    };
    let kind = code
        .and_then(settled)
        .or_else(|| by_code(error))
        .or_else(|| named.and_then(settled))
        .or_else(|| by_phrase(&text))
        .unwrap_or(match code.or(named) {
            // A 404 that names no missing model is more often a wrong base
            // URL, which no other model would mend.
            Some(404) => FailureKind::Settings,
            Some(400..=499) => FailureKind::BadRequest,
            _ => FailureKind::Unknown,
        });
    let overflow = match kind {
        FailureKind::ContextTooLong => Some(limits(&text, error)),
        // A reply asked for that is longer than the model writes is turned
        // down as a bad request, and only its numbers say why.
        FailureKind::BadRequest => {
            Some(limits(&text, error)).filter(|o| o.output.is_some() || o.context.is_some())
        }
        _ => None,
    };
    Failure {
        kind,
        retry_after: retry_after(headers),
        overflow,
        stands: false,
        out_of_room: false,
        gateway: None,
        // A gateway whose upstream account is spent answers 503, as one did
        // with "every lender we tried was refused by Anthropic on their API
        // credit balance": waiting only sent the same request again, twenty
        // times in a run nobody watched, where another gateway could have
        // answered at once. It stays the server's error, not the caller's
        // credit: Letronna's API relays its own pool's spent credit this way
        // on purpose, and a gateway's lenders are not the caller's key.
        lasting: kind == FailureKind::Server && by_phrase(&text) == Some(FailureKind::Quota),
        message: words(status, body, raw),
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
    ("unknown model", FailureKind::NotFound),
    ("not a valid model", FailureKind::NotFound),
];

/// The kind the first of `PHRASES` found in the lowercase `text` names.
fn by_phrase(text: &str) -> Option<FailureKind> {
    PHRASES
        .iter()
        .find(|(phrase, _)| text.contains(phrase))
        .map(|(_, kind)| *kind)
}

/// The first HTTP error status the lowercase `text` names, as "(http 502)",
/// "status 503" or "status code 429" do, of those that say nothing of the
/// key, the credit or the base URL. A status in the words of an error from a
/// provider that had taken the request is its upstream's, so a 401 or a 404
/// there is not the user's to mend.
fn status_named(text: &str) -> Option<u16> {
    [
        "http ",
        "status code ",
        "status code: ",
        "status ",
        "status: ",
    ]
    .into_iter()
    .flat_map(|phrase| {
        text.match_indices(phrase)
            .map(move |(at, _)| (at, at + phrase.len()))
    })
    .filter_map(|(at, start)| {
        let digits = text.get(start..start + 3)?;
        let after = text[start + 3..].chars().next();
        let code: u16 = digits.parse().ok()?;
        (matches!(code, 400 | 405..=599) && !after.is_some_and(|c| c.is_ascii_digit()))
            .then_some((at, code))
    })
    .min_by_key(|(at, _)| *at)
    .map(|(_, code)| code)
}

/// The limits an overflow error names, read from its lowercase `text` and
/// from the numbers llama.cpp puts beside its words.
fn limits(text: &str, error: Option<&Value>) -> Overflow {
    let field = |name: &str| error.and_then(|e| e.get(name)?.as_u64());
    let nth = |phrase: &str, n: usize| numbers(text, phrase).get(n).copied();
    Overflow {
        // OpenAI, OpenRouter and vLLM, then Anthropic's two.
        context: field("n_ctx")
            .or_else(|| nth("maximum context length is", 0))
            .or_else(|| nth("prompt is too long", 1))
            .or_else(|| nth("exceed context limit", 2)),
        input: field("n_prompt_tokens")
            .or_else(|| nth("resulted in", 0))
            .or_else(|| nth("request has", 0))
            .or_else(|| before(text, " of text input"))
            .or_else(|| before(text, " in the messages"))
            .or_else(|| nth("prompt is too long", 0))
            .or_else(|| nth("exceed context limit", 0)),
        // OpenAI's, then Anthropic's "max_tokens: 64000 > 32000, which is
        // the maximum allowed number of output tokens".
        output: nth("supports at most", 0).or_else(|| {
            text.contains("maximum allowed number of output tokens")
                .then(|| nth("max_tokens:", 1))
                .flatten()
        }),
    }
}

/// Whether a refusal with `status` and `body` says the request was more
/// bytes than the server takes, which a shorter window may not mend when
/// pictures are most of it; with the most it takes, when it names that.
/// Some(None) is too big by a limit not named.
pub fn oversize(status: u16, body: &Value) -> Option<Option<u64>> {
    let error = body.get("error");
    let text = error.unwrap_or(body).to_string().to_lowercase();
    // A request over a limit on tokens a minute, which Groq turns down with
    // a 413 too, says nothing of how many bytes the gateway takes. OpenRouter
    // passes Groq's on under a numeric code, with Groq's own only in the copy
    // it keeps in `metadata.raw`.
    if by_code(error) == Some(FailureKind::RateLimit) || text.contains("rate_limit_exceeded") {
        return None;
    }
    let named = ["code", "type"]
        .iter()
        .any(|field| error.and_then(|e| e.get(field)?.as_str()) == Some("request_too_large"));
    // What a 413 is called, for a gateway that passes one on under another
    // status. Not "request too large": OpenAI says that of a rate limit.
    let said = ["payload too large", "entity too large"]
        .iter()
        .any(|phrase| text.contains(phrase));
    (status == 413 || named || said).then(|| size_named(&text))
}

/// The first size in bytes the lowercase `text` names, such as the 8 MB of
/// "exceeds the 8 MB limit". A kB or MB is read as thousands, as it says,
/// so where a server meant 1024 the limit is read low, never high.
fn size_named(text: &str) -> Option<u64> {
    let mut rest = text;
    while let Some(at) = rest.find(|c: char| c.is_ascii_digit()) {
        rest = &rest[at..];
        // Digits grouped by commas too, as in 10,485,760 bytes.
        let end = rest
            .char_indices()
            .find(|&(i, c)| {
                !(c.is_ascii_digit()
                    || (matches!(c, '.' | ',')
                        && rest[i + 1..].starts_with(|c: char| c.is_ascii_digit())))
            })
            .map_or(rest.len(), |(i, _)| i);
        let (number, after) = rest.split_at(end);
        rest = after;
        let unit = after.trim_start();
        let unit = &unit[..unit
            .find(|c: char| !c.is_ascii_alphabetic())
            .unwrap_or(unit.len())];
        // Not a bare "b", which is also a model's size, as in llama-3-70b.
        let scale: u64 = match unit {
            "byte" | "bytes" => 1,
            "kb" => 1_000,
            "kib" => 1 << 10,
            "mb" => 1_000_000,
            "mib" => 1 << 20,
            "gb" => 1_000_000_000,
            "gib" => 1 << 30,
            _ => continue,
        };
        let Ok(number) = number.replace(',', "").parse::<f64>() else {
            continue;
        };
        let bytes = (number * scale as f64) as u64;
        if bytes > 0 {
            return Some(bytes);
        }
    }
    None
}

/// The numbers in the clause of `text` that `phrase` begins, in order.
fn numbers(text: &str, phrase: &str) -> Vec<u64> {
    let Some(at) = text.find(phrase) else {
        return Vec::new();
    };
    let rest = &text[at + phrase.len()..];
    // A quote ends the clause too: the text is the error's JSON.
    let clause = &rest[..rest.find([',', '.', '"']).unwrap_or(rest.len())];
    clause
        .split(|c: char| !c.is_ascii_digit())
        .filter_map(|n| n.parse().ok())
        .collect()
}

/// The number just before the first `phrase` in `text`.
fn before(text: &str, phrase: &str) -> Option<u64> {
    let head = &text[..text.find(phrase)?];
    let digits = head.trim_end_matches(|c: char| c.is_ascii_digit());
    head[digits.len()..].parse().ok()
}

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

/// Characters kept of what a reply that is no JSON said: a plain-text error
/// fits, a whole page does not.
const PAGE_CHARS: usize = 300;

/// What the user is told: the provider's own complaint when it sent one, or
/// else what its status means, then what a reply that is no JSON, `raw`,
/// said, which is often the one clue to which server failed and why.
fn words(status: Option<u16>, body: &Value, raw: &str) -> String {
    if let Err(message) = crate::chat::unwrap_response(body) {
        return message;
    }
    let Some(code) = status else {
        return "The provider could not finish this response.".to_owned();
    };
    let meant = if reqwest::StatusCode::from_u16(code).is_ok_and(crate::chat::transient) {
        format!(
            "The gateway is having trouble right now (HTTP {code}). This is at \
             the provider's end, not your settings. Try again in a moment, or \
             switch gateway in Settings."
        )
    } else {
        format!("Provider returned HTTP {code}. Check connection settings and quota.")
    };
    // A reply in JSON with no `error`, such as FastAPI's `detail`, says it
    // where its server puts it, which only the status would otherwise be
    // left of.
    let said = match body {
        Value::Null => page_words(raw),
        body => crate::chat::error_words(body).and_then(|words| page_words(&words)),
    };
    match said {
        Some(said) => format!("{meant} The provider said: {said}"),
        None => meant,
    }
}

/// What a reply that is no JSON says, on one line and cut short: an HTML
/// page's title, since the rest of the page is markup, or else its text.
/// `None` when it says nothing, and for a page with no title and a style or
/// a script, whose text would be mostly those.
fn page_words(raw: &str) -> Option<String> {
    let raw = raw.trim();
    let text = if raw.starts_with('<') {
        // Lowercased letter for letter, so its places are the page's too.
        let lower = raw.to_ascii_lowercase();
        let titled = lower.find("<title").and_then(|at| {
            let start = at + lower[at..].find('>')? + 1;
            let end = start + lower[start..].find("</title")?;
            Some(raw[start..end].to_owned())
        });
        match titled {
            Some(title) => title,
            None if lower.contains("<script") || lower.contains("<style") => return None,
            None => untagged(raw),
        }
    } else {
        raw.to_owned()
    };
    // A control character could break the row the words are shown in, or
    // reach a terminal as a command.
    let flat = (text.chars())
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    match flat.char_indices().nth(PAGE_CHARS) {
        _ if flat.is_empty() => None,
        Some((cut, _)) => Some(format!("{}…", &flat[..cut])),
        None => Some(flat),
    }
}

/// The text of `html` with its tags left out.
fn untagged(html: &str) -> String {
    let mut text = String::new();
    let mut in_tag = false;
    for c in html.chars() {
        match c {
            // A space for each tag, so the words either side stay apart.
            '<' => {
                in_tag = true;
                text.push(' ');
            }
            '>' => in_tag = false,
            c if !in_tag => text.push(c),
            _ => {}
        }
    }
    text
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
    fn a_status_given_only_in_words_is_read() {
        assert_eq!(
            kind(
                None,
                r#"{"error":{"message":"every lender we tried refused this request (HTTP 502)","type":"upstream_error","code":null}}"#
            ),
            FailureKind::Server
        );
        assert_eq!(
            kind(None, r#"{"error":{"message":"Upstream status code 429"}}"#),
            FailureKind::RateLimit
        );
        assert_eq!(
            kind(None, r#"{"error":{"message":"refused (HTTP 400)"}}"#),
            FailureKind::BadRequest
        );
        assert_eq!(
            kind(None, r#"{"error":{"message":"refused (HTTP 422)"}}"#),
            FailureKind::BadRequest
        );
        // TGI's, its status in a field of its own.
        assert_eq!(
            kind(
                None,
                r#"{"error":{"message":"Input validation error: `inputs` tokens + `max_new_tokens` must be <= 4096","http_status_code":422}}"#
            ),
            FailureKind::BadRequest
        );
        // The upstream's 401 and 404 say nothing of the user's key or base
        // URL, which the gateway has just accepted.
        assert_eq!(
            kind(None, r#"{"error":{"message":"refused (HTTP 401)"}}"#),
            FailureKind::Unknown
        );
        assert_eq!(
            kind(None, r#"{"error":{"message":"refused (HTTP 404)"}}"#),
            FailureKind::Unknown
        );
        // A status the reply came with, or its code, is not overruled.
        assert_eq!(
            kind(Some(400), r#"{"error":{"message":"refused (HTTP 502)"}}"#),
            FailureKind::BadRequest
        );
        assert_eq!(
            kind(
                None,
                r#"{"error":{"message":"refused (HTTP 502)","code":"rate_limit_exceeded"}}"#
            ),
            FailureKind::RateLimit
        );
        assert_eq!(status_named("http 1.1 at host:5000"), None);
        assert_eq!(status_named("status 2000, then http 5023"), None);
        assert_eq!(status_named("status 200 after status code 404"), None);
        assert_eq!(status_named("status: 503 from port 8080"), Some(503));
        assert_eq!(status_named("status 400 after http 503"), Some(400));
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
            // OpenRouter says a model id it does not know with 400.
            (
                Some(400),
                r#"{"error":{"message":"acme/nope is not a valid model ID","code":400}}"#,
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
        // So is one that gives it as a bare string.
        assert_eq!(
            said(Some(400), r#"{"error":"model 'qwen3' is not loaded"}"#),
            "model 'qwen3' is not loaded"
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
        assert_eq!(
            said(Some(502), ""),
            "The gateway is having trouble right now (HTTP 502). This is at the provider's \
             end, not your settings. Try again in a moment, or switch gateway in Settings."
        );
    }

    #[test]
    fn a_server_error_that_names_a_spent_credit_balance_is_not_waited_out() {
        // As a gateway relaying Anthropic's refusal of its own account said it.
        let body = serde_json::json!({"error": {
            "message": "every lender we tried was refused by Anthropic on their API credit \
                        balance (exhausted, or plan-included credits, which do not cover \
                        requests that identify as Claude Code)",
            "type": "upstream_error",
            "code": null,
        }});
        let failure = classify(Some(503), &HeaderMap::new(), &body);
        // The gateway's own credit, not the caller's: no Add credit or
        // Upgrade is offered for it, hosted or not.
        assert_eq!(failure.kind, FailureKind::Server);
        assert!(failure.lasting);
        assert!(crate::retry::after(&failure, 1, true, None).is_none());
        assert!(crate::fallback::mends(&failure));
        assert!(failure.to_string().contains("credit balance (exhausted"));
        // A 503 that says nothing of credit is waited out as before.
        let busy = serde_json::json!({"error": {"message": "upstream timed out", "type": "upstream_error"}});
        let busy = classify(Some(503), &HeaderMap::new(), &busy);
        assert_eq!(busy.kind, FailureKind::Server);
        assert!(!busy.lasting && crate::retry::after(&busy, 1, true, None).is_some());
        // And a 402 that says so is still the caller's.
        let own = serde_json::json!({"error": {"message": "Your credit balance is too low"}});
        assert_eq!(
            classify(Some(402), &HeaderMap::new(), &own).kind,
            FailureKind::Quota
        );
    }

    #[test]
    fn a_gateway_with_no_lender_for_an_hour_is_waited_out_only_by_a_timed_run_nobody_watches() {
        // As pool.example answered through a DeepSWE round, for about an hour.
        let body = serde_json::json!({"error": {
            "message": "no lender was eligible to serve this request",
            "type": "upstream_error",
        }});
        let mut headers = HeaderMap::new();
        headers.insert(RETRY_AFTER, "3788".parse().unwrap());
        let failure = classify(Some(503), &headers, &body);
        assert!(!failure.lasting);
        let deadline = std::time::Instant::now() + Duration::from_secs(30 * 60);
        let retry = crate::retry::after(&failure, 1, true, Some(deadline)).unwrap();
        assert!(retry.wait < Duration::from_secs(60), "{:?}", retry.wait);
        assert!(crate::retry::after(&failure, 1, true, None).is_none());
        assert!(crate::retry::after(&failure, 1, false, Some(deadline)).is_none());
    }

    #[test]
    fn what_a_reply_that_is_not_json_said_is_kept() {
        let said = |status, raw: &str| {
            let body: Value = serde_json::from_str(raw).unwrap_or_default();
            classify_text(Some(status), &HeaderMap::new(), &body, raw).to_string()
        };
        assert_eq!(
            said(404, "404 page not found\n"),
            "Provider returned HTTP 404. Check connection settings and quota. \
             The provider said: 404 page not found"
        );
        // Envoy's, over two lines.
        let envoy = "upstream connect error or disconnect/reset before headers.\r\n  \
                     reset reason: overflow";
        assert!(said(502, envoy).ends_with(
            "switch gateway in Settings. The provider said: upstream connect error or \
             disconnect/reset before headers. reset reason: overflow"
        ));
        // Of a page, its title, or its words when it is no more than those.
        assert!(
            said(
                502,
                "<!DOCTYPE html><html><head><title>example.com | 502: Bad gateway</title>\
                 <style>body{margin:0}</style></head><body>...</body></html>"
            )
            .ends_with("The provider said: example.com | 502: Bad gateway")
        );
        assert!(
            said(
                503,
                "<html><body><h1>Service Unavailable</h1></body></html>"
            )
            .ends_with("The provider said: Service Unavailable")
        );
        assert!(
            said(503, "<html><script>var page = 1;</script></html>")
                .ends_with("switch gateway in Settings.")
        );
        // Cut short, and with nothing that could reach a terminal as a command.
        let long = said(500, &format!("\u{1b}[2Jtrace: {}", "frame ".repeat(100)));
        let quoted = long.split("The provider said: ").nth(1).unwrap();
        assert_eq!(quoted.chars().count(), PAGE_CHARS + 1);
        assert!(quoted.starts_with("[2Jtrace: frame") && quoted.ends_with('…'));
        // A reply in JSON is read for its words wherever they are, as
        // `chat::error_words` finds them.
        assert_eq!(
            said(422, r#"{"detail":"model is not loaded"}"#),
            "Provider returned HTTP 422. Check connection settings and quota. \
             The provider said: model is not loaded"
        );
        assert!(said(400, r#"{"message":"bad model"}"#).ends_with("The provider said: bad model"));
        assert_eq!(
            said(
                422,
                r#"{"detail":[{"loc":["body","model"],"msg":"no such model"}]}"#
            ),
            "Provider returned HTTP 422. Check connection settings and quota. \
             The provider said: body.model: no such model"
        );
        assert_eq!(
            said(418, "{}"),
            "Provider returned HTTP 418. Check connection settings and quota."
        );
        // The kind is the status's, whatever the page says.
        let body = Value::Null;
        let page = classify_text(Some(404), &HeaderMap::new(), &body, "404 page not found");
        assert_eq!(page.kind, FailureKind::Settings);
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

    #[test]
    fn each_overflow_wording_gives_the_numbers_it_names() {
        let limits = |context, input, output| Overflow {
            context,
            input,
            output,
        };
        let cases = [
            // OpenAI, now and before.
            (
                400,
                r#"{"error":{"message":"This model's maximum context length is 128000 tokens. However, your messages resulted in 130532 tokens. Please reduce the length of the messages.","type":"invalid_request_error","param":"messages","code":"context_length_exceeded"}}"#,
                FailureKind::ContextTooLong,
                limits(Some(128_000), Some(130_532), None),
            ),
            (
                400,
                r#"{"error":{"message":"This model's maximum context length is 8192 tokens. However, you requested 8500 tokens (7000 in the messages, 1500 in the completion). Please reduce the length of the messages or completion.","type":"invalid_request_error","param":"messages","code":"context_length_exceeded"}}"#,
                FailureKind::ContextTooLong,
                limits(Some(8_192), Some(7_000), None),
            ),
            (
                400,
                r#"{"error":{"message":"max_tokens is too large: 50000. This model supports at most 16384 completion tokens, whereas you provided 50000.","type":"invalid_request_error","param":"max_tokens","code":null}}"#,
                FailureKind::BadRequest,
                limits(None, None, Some(16_384)),
            ),
            // OpenRouter's own, and Anthropic's through it.
            (
                400,
                r#"{"error":{"message":"This endpoint's maximum context length is 200000 tokens. However, you requested about 213000 tokens (204808 of text input, 8192 in the output). Please reduce the length of either one.","code":400}}"#,
                FailureKind::ContextTooLong,
                limits(Some(200_000), Some(204_808), None),
            ),
            (
                400,
                r#"{"error":{"message":"Provider returned error","code":400,"metadata":{"raw":"{\"type\":\"error\",\"error\":{\"type\":\"invalid_request_error\",\"message\":\"prompt is too long: 208437 tokens > 200000 maximum\"}}","provider_name":"Anthropic"}}}"#,
                FailureKind::ContextTooLong,
                limits(Some(200_000), Some(208_437), None),
            ),
            (
                400,
                r#"{"type":"error","error":{"type":"invalid_request_error","message":"input length and `max_tokens` exceed context limit: 188240 + 21333 > 200000, decrease input length or `max_tokens` and try again"}}"#,
                FailureKind::BadRequest,
                limits(Some(200_000), Some(188_240), None),
            ),
            (
                400,
                r#"{"type":"error","error":{"type":"invalid_request_error","message":"max_tokens: 64000 > 32000, which is the maximum allowed number of output tokens for claude-opus-4-20250514"}}"#,
                FailureKind::BadRequest,
                limits(None, None, Some(32_000)),
            ),
            // vLLM, for a long prompt and for a long reply.
            (
                400,
                r#"{"object":"error","message":"This model's maximum context length is 32768 tokens. However, you requested 33792 tokens (25600 in the messages, 8192 in the completion). Please reduce the length of the messages or completion.","type":"BadRequestError","param":null,"code":400}"#,
                FailureKind::ContextTooLong,
                limits(Some(32_768), Some(25_600), None),
            ),
            (
                400,
                r#"{"error":{"message":"'max_tokens' or 'max_completion_tokens' is too large: 8192. This model's maximum context length is 32768 tokens and your request has 30000 input tokens (8192 > 32768 - 30000).","type":"BadRequestError","param":null,"code":400}}"#,
                FailureKind::ContextTooLong,
                limits(Some(32_768), Some(30_000), None),
            ),
            // llama.cpp, with its numbers beside the words and before it had them.
            (
                400,
                r#"{"error":{"code":400,"message":"the request exceeds the available context size, try increasing it","type":"exceed_context_size_error","n_prompt_tokens":4589,"n_ctx":4096}}"#,
                FailureKind::ContextTooLong,
                limits(Some(4_096), Some(4_589), None),
            ),
            (
                400,
                r#"{"error":{"code":400,"message":"the request exceeds the available context size. try increasing the context size or enable context shift","type":"invalid_request_error"}}"#,
                FailureKind::ContextTooLong,
                Overflow::default(),
            ),
            // Ollama answers with a 500, and a proxy's size limit says nothing.
            (
                500,
                r#"{"error":{"message":"the input length exceeds the context length","type":"api_error","param":null,"code":null}}"#,
                FailureKind::ContextTooLong,
                Overflow::default(),
            ),
            (413, "{}", FailureKind::ContextTooLong, Overflow::default()),
        ];
        for (status, body, expected, numbers) in cases {
            let body: Value = serde_json::from_str(body).unwrap();
            let failure = classify(Some(status), &HeaderMap::new(), &body);
            assert_eq!(failure.kind, expected, "{body}");
            assert_eq!(failure.overflow, Some(numbers), "{body}");
        }
    }

    #[test]
    fn a_failed_turn_is_kept_with_when_to_come_back_and_a_stopped_one_is_not() {
        let mut limited = Failure::new(FailureKind::RateLimit, "Slow down.");
        limited.retry_after = Some(Duration::from_secs(300));
        let message = limited.kept("m", true, 1_000).unwrap();
        assert_eq!(
            (message.role.as_str(), message.content.as_str()),
            (ROLE, "Slow down.")
        );
        assert_eq!(message.model, "m");
        assert_eq!(
            message.failed.as_deref(),
            Some(&Kept {
                kind: FailureKind::RateLimit,
                until: Some(1_300),
                hosted: true,
                upgrade: false,
                elsewhere: false,
            })
        );
        // A wait asked with a bad key is no promise it will pass.
        let mut refused = Failure::new(FailureKind::Auth, "Bad key.");
        refused.retry_after = Some(Duration::from_secs(300));
        let message = refused.kept("m", false, 1_000).unwrap();
        assert_eq!(message.failed.unwrap().until, None);
        // A wait of years is no time anyone could be offered to come back at.
        limited.retry_after = Some(Duration::from_secs(1_000_000_000_000_000_000));
        let message = limited.kept("m", false, 1_000).unwrap();
        assert_eq!(message.failed.unwrap().until, None);
        assert!(
            Failure::new(FailureKind::Stopped, "Stopped.")
                .kept("m", false, 1_000)
                .is_none()
        );
    }

    /// A failure of `kind` kept as `kept` gives it, hosted or not.
    fn kept(kind: FailureKind, until: Option<u64>, hosted: bool) -> Kept {
        Kept {
            kind,
            until,
            hosted,
            upgrade: false,
            elsewhere: false,
        }
    }

    #[test]
    fn the_likely_fix_comes_first_and_there_are_never_more_than_two() {
        let fixes = |kind, until, hosted| kept(kind, until, hosted).fixes(1_000);
        use Fix::*;
        assert_eq!(
            fixes(FailureKind::Quota, None, true),
            [AddCredit, CopyDetails]
        );
        assert_eq!(fixes(FailureKind::Quota, None, false), [CopyDetails]);
        assert_eq!(fixes(FailureKind::Auth, None, true), [SignIn, CopyDetails]);
        assert_eq!(fixes(FailureKind::Auth, None, false), [CopyDetails]);
        assert_eq!(
            fixes(FailureKind::RateLimit, Some(1_300), false),
            [RetryAt(1_300), CopyDetails]
        );
        // Once the wait is over there is nothing left to wait for.
        assert_eq!(
            fixes(FailureKind::RateLimit, Some(900), false),
            [Retry, CopyDetails]
        );
        assert_eq!(
            fixes(FailureKind::Network, None, false),
            [Retry, CopyDetails]
        );
        assert_eq!(
            fixes(FailureKind::NotFound, None, true),
            [ChooseModel, CopyDetails]
        );
        assert_eq!(
            fixes(FailureKind::ContextTooLong, None, true),
            [ChooseModel, CopyDetails]
        );
        assert_eq!(fixes(FailureKind::Settings, None, false), [CopyDetails]);
        assert_eq!(
            fixes(FailureKind::Unknown, None, false),
            [Retry, CopyDetails]
        );
    }

    fn account(plan: &str, credits: i64) -> crate::auth::Account {
        crate::auth::Account {
            plan: plan.into(),
            credits,
            ..Default::default()
        }
    }

    #[test]
    fn a_free_plan_out_of_credit_is_offered_a_subscription_not_settings() {
        use Fix::*;
        let free = account("free", 0);
        let fixes = |kind, signed_in: Option<&crate::auth::Account>, elsewhere| {
            let mut kept = kept(kind, None, true);
            kept.knowing(signed_in, elsewhere);
            (kept.fixes(1_000), kept.cause(|at| at.to_string()))
        };
        let quota = FailureKind::Quota;
        assert_eq!(
            fixes(quota, Some(&free), false),
            (vec![Upgrade, CopyDetails], "Out of credit".into())
        );
        // With a key of their own set up, a model there goes on now.
        assert_eq!(fixes(quota, Some(&free), true).0, [Upgrade, ChooseModel]);
        // Credit left, or a paid plan, is topped up rather than upgraded.
        assert_eq!(
            fixes(quota, Some(&account("free", 5)), false).0,
            [AddCredit, CopyDetails]
        );
        assert_eq!(
            fixes(quota, Some(&account("pro", 0)), true).0,
            [AddCredit, ChooseModel]
        );
        // A model the Free plan does not include is one a subscription does.
        let gated = fixes(FailureKind::Auth, Some(&account("free", 5)), false);
        assert_eq!(
            gated,
            (vec![Upgrade, CopyDetails], "Needs a subscription".into())
        );
        // Only with nobody signed in is a sign-in the way out.
        assert_eq!(
            fixes(FailureKind::Auth, None, true),
            (vec![SignIn, CopyDetails], "Signed out".into())
        );
        // A failure on a key of the user's own is not the account's.
        let mut own = kept(quota, None, false);
        own.knowing(Some(&free), true);
        assert_eq!((own.upgrade, own.elsewhere), (false, false));
        assert_eq!(own.fixes(1_000), [CopyDetails]);
    }

    #[test]
    fn only_a_free_plan_with_nothing_left_needs_an_upgrade() {
        assert!(upgrade(FailureKind::Quota, Some(&account("free", 0))));
        assert!(upgrade(FailureKind::Quota, Some(&account("free", -3))));
        assert!(!upgrade(FailureKind::Quota, Some(&account("free", 1))));
        assert!(!upgrade(FailureKind::Quota, Some(&account("pro", 0))));
        // An account never checked has no plan to go by.
        assert!(!upgrade(FailureKind::Quota, Some(&account("", 0))));
        assert!(!upgrade(FailureKind::Quota, None));
        // Billed to a team, the team's pool is what pays.
        let mut billed = account("free", 0);
        billed.team = Some(crate::auth::Team {
            credits: 500,
            ..Default::default()
        });
        assert!(!upgrade(FailureKind::Quota, Some(&billed)));
        assert!(!upgrade(FailureKind::Server, Some(&account("free", 0))));
    }

    #[test]
    fn the_account_is_asked_again_when_the_hosted_pool_wants_paying() {
        let hosted = Some(crate::gateway::LETRONNA);
        assert!(ask_account_again(hosted, FailureKind::Quota));
        assert!(ask_account_again(hosted, FailureKind::Auth));
        assert!(!ask_account_again(hosted, FailureKind::Overloaded));
        assert!(!ask_account_again(Some("OpenAI"), FailureKind::Quota));
        assert!(!ask_account_again(None, FailureKind::Auth));
    }

    #[test]
    fn the_cause_is_said_in_a_few_plain_words() {
        let cause = |kind, until, hosted| kept(kind, until, hosted).cause(|at| format!("at {at}"));
        assert_eq!(cause(FailureKind::Quota, None, true), "Out of credit");
        assert_eq!(
            cause(FailureKind::RateLimit, Some(1_300), false),
            "Rate limited until at 1300"
        );
        assert_eq!(cause(FailureKind::RateLimit, None, false), "Rate limited");
        assert_eq!(cause(FailureKind::NotFound, None, false), "Model not found");
        assert_eq!(
            cause(FailureKind::ContextTooLong, None, false),
            "Context too long"
        );
        assert_eq!(cause(FailureKind::Auth, None, true), "Signed out");
        assert_eq!(
            cause(FailureKind::Network, None, false),
            "Gateway unreachable"
        );
    }

    #[test]
    fn a_kept_failure_reads_back_as_it_was_written() {
        let message = Failure::new(FailureKind::ContextTooLong, "Too long.")
            .kept("m", false, 1_000)
            .unwrap();
        let line = serde_json::to_string(&message).unwrap();
        assert!(
            line.contains(r#""failed":{"kind":"context_too_long"}"#),
            "{line}"
        );
        let back: Message = serde_json::from_str(&line).unwrap();
        assert_eq!(back.failed, message.failed);
        // A kind this version does not know keeps its message.
        let newer: Message = serde_json::from_str(
            r#"{"role":"failure","content":"Odd.","failed":{"kind":"moon_phase"}}"#,
        )
        .unwrap();
        assert_eq!(newer.failed.unwrap().kind, FailureKind::Unknown);
        assert_eq!(details(&back), "ContextTooLong from m: Too long.");
    }

    #[test]
    fn a_bad_request_that_names_no_limit_is_no_overflow() {
        let body = serde_json::json!({"error": {"message": "Invalid value for 'temperature': 3", "type": "invalid_request_error"}});
        let failure = classify(Some(400), &HeaderMap::new(), &body);
        assert_eq!(failure.kind, FailureKind::BadRequest);
        assert_eq!(failure.overflow, None);
    }

    #[test]
    fn a_body_too_big_is_told_from_too_many_tokens_with_the_limit_it_names() {
        let oversize = |status, body: &str| {
            let body = serde_json::from_str(body).unwrap_or_default();
            super::oversize(status, &body)
        };
        // A gateway's, which is still an overflow, in its own words.
        let gateway = r#"{"error":{"message":"Request body exceeds the 8 MB limit.","type":"invalid_request_error","code":"request_too_large"}}"#;
        assert_eq!(oversize(413, gateway), Some(Some(8_000_000)));
        let failure = classify(
            Some(413),
            &HeaderMap::new(),
            &serde_json::from_str(gateway).unwrap(),
        );
        assert_eq!(failure.kind, FailureKind::ContextTooLong);
        assert_eq!(failure.to_string(), "Request body exceeds the 8 MB limit.");
        // Anthropic's names no limit, nor does a proxy's page.
        let anthropic = r#"{"type":"error","error":{"type":"request_too_large","message":"Request exceeds the maximum allowed number of bytes."}}"#;
        assert_eq!(oversize(413, anthropic), Some(None));
        assert_eq!(
            oversize(413, "<html>413 Request Entity Too Large</html>"),
            Some(None)
        );
        // One passed on as a bad gateway, in the words of a 413.
        let passed_on =
            r#"{"error":{"message":"upstream: 413 Payload Too Large (max 10,485,760 bytes)"}}"#;
        assert_eq!(oversize(502, passed_on), Some(Some(10_485_760)));
        // Too many tokens, and a rate limit OpenAI words as a request too large.
        let tokens = r#"{"error":{"message":"This model's maximum context length is 16000 tokens.","type":"invalid_request_error","code":"context_length_exceeded"}}"#;
        assert_eq!(oversize(400, tokens), None);
        let rate = r#"{"error":{"message":"Request too large for gpt-4o on tokens per min (TPM): Limit 30000, Requested 50000.","type":"tokens","code":"rate_limit_exceeded"}}"#;
        assert_eq!(oversize(429, rate), None);
        // Groq's for the same, which comes as a 413 and is still refitted.
        let groq = r#"{"error":{"message":"Request too large for model `llama-4-scout` on tokens per minute (TPM): Limit 8000, Requested 33000, please reduce your message size and try again.","type":"tokens","code":"rate_limit_exceeded"}}"#;
        assert_eq!(oversize(413, groq), None);
        let relayed = r#"{"error":{"message":"Provider returned error","code":413,"metadata":{"raw":"{\"error\":{\"message\":\"Request too large for model `llama-4-scout` on tokens per minute (TPM): Limit 8000, Requested 33000.\",\"type\":\"tokens\",\"code\":\"rate_limit_exceeded\"}}","provider_name":"Groq"}}}"#;
        assert_eq!(oversize(413, relayed), None);
        let failure = classify(
            Some(413),
            &HeaderMap::new(),
            &serde_json::from_str(groq).unwrap(),
        );
        assert_eq!(failure.kind, FailureKind::ContextTooLong);
    }

    #[test]
    fn a_size_is_read_in_the_unit_it_is_given_in() {
        assert_eq!(size_named("at most 10485760 bytes"), Some(10_485_760));
        assert_eq!(size_named("larger than 8 mib"), Some(8 << 20));
        assert_eq!(size_named("413: over the 7.5mb limit"), Some(7_500_000));
        assert_eq!(size_named("max 512 kb, see rfc 9110"), Some(512_000));
        assert_eq!(size_named("\"code\":413,\"status\":\"too big\""), None);
        assert_eq!(size_named("too large for llama-3.1-70b"), None);
    }
}
