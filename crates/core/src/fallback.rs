//! Carrying a task on with another model when the chosen one cannot answer.
//!
//! Retries wait out what passes in seconds. An overload that lasts, an
//! account out of credit or a model the gateway no longer serves still ended
//! the task part way through. `Settings.fallbacks` names models to go on
//! with, in order, each on a gateway of its own or on the session's. One is
//! asked only once the chosen model's own tries are spent, and only after a
//! failure another model could mend: never a stop, a request turned down as
//! written, or a reply the provider refused, which would go the same way
//! anywhere.
//!
//! A model that failed is passed over for `COOLDOWN`, remembered in memory
//! only, so each request after it goes straight to the fallback rather than
//! sitting through the same retries again. Once that is over the chosen
//! model is asked first again.

use crate::chat::Connection;
use crate::failure::{Failure, FailureKind};
use std::collections::{BTreeMap, HashMap};
use std::fmt;
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};

/// How long a model that failed is passed over.
pub const COOLDOWN: Duration = Duration::from_secs(5 * 60);

/// When each (endpoint, model) that failed may be asked again.
static COOLING: Mutex<BTreeMap<(String, String), Instant>> = Mutex::new(BTreeMap::new());

/// A model to go on with, as written in Settings: `model`, or `Gateway:model`
/// for one on another gateway.
#[derive(Clone, Debug, PartialEq)]
pub struct Fallback {
    /// Empty for the session's own gateway.
    pub gateway: String,
    pub model: String,
}

impl Fallback {
    /// Read `model` or `Gateway:model`. A prefix that names no gateway is
    /// part of the model id, as the tag in Ollama's `llama3:8b` is.
    pub fn parse(text: &str) -> Result<Self, String> {
        let text = text.trim();
        let named = text.split_once(':').and_then(|(gateway, model)| {
            let gateway = crate::gateway::GATEWAYS
                .iter()
                .find(|g| g.name.eq_ignore_ascii_case(gateway.trim()))?;
            Some((gateway.name.to_owned(), model.trim().to_owned()))
        });
        let (gateway, model) = named.unwrap_or_else(|| (String::new(), text.to_owned()));
        if model.is_empty() {
            return Err(format!(
                "\"{text}\" names no model. Write a model id, or Gateway:model for one on another gateway."
            ));
        }
        Ok(Self { gateway, model })
    }

    /// Every entry of `list` that reads, or the first that does not.
    pub fn parse_all(list: &[String]) -> Result<Vec<Self>, String> {
        list.iter().map(|text| Self::parse(text)).collect()
    }

    /// Where requests for it go, given the session's connection: its own
    /// gateway's address and key, or the session's when it names none.
    fn connect(
        &self,
        primary: &Connection,
        keys: &HashMap<String, String>,
        custom: &str,
    ) -> Connection {
        let (endpoint, key) = if self.gateway.is_empty() {
            (primary.endpoint.clone(), primary.key.clone())
        } else {
            let key = keys
                .get(crate::gateway::key_slot(&self.gateway))
                .cloned()
                .unwrap_or_default();
            crate::gateway::resolve(&self.gateway, custom, &key)
        };
        Connection {
            endpoint,
            key,
            model: self.model.clone(),
            ..primary.clone()
        }
    }
}

impl fmt::Display for Fallback {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.gateway.as_str() {
            "" => f.write_str(&self.model),
            gateway => write!(f, "{gateway}:{}", self.model),
        }
    }
}

/// A model asked in place of the session's, as the frontends are told of it.
#[derive(Clone, Debug, PartialEq)]
pub struct Switch {
    /// The model asked instead, as its reply is labelled.
    pub model: String,
    /// What the notice calls it: the model, or the account it is asked on
    /// when that is all that changed.
    pub label: String,
    /// Where it is served, so its reply is priced there.
    pub endpoint: String,
    /// The model given up on and why, when one failed on this request; `None`
    /// when this request went to the fallback because the one it would have
    /// asked is still cooling down, which the user was told of already.
    pub after: Option<(String, FailureKind)>,
}

impl fmt::Display for Switch {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Switched to {}", self.label)?;
        let Some((from, kind)) = &self.after else {
            return Ok(());
        };
        let why = match kind {
            FailureKind::Overloaded => "is overloaded",
            FailureKind::Quota => "is out of credit or quota",
            FailureKind::NotFound => "was not found",
            FailureKind::Auth => "turned its key away",
            FailureKind::Settings => "is not set up",
            FailureKind::Network => "could not be reached",
            FailureKind::RateLimit => "is rate limited",
            FailureKind::ContextTooLong => "cannot read this much at once",
            _ => "had a server error",
        };
        let minutes = COOLDOWN.as_secs() / 60;
        write!(f, ": {from} {why}. It is asked again in {minutes} minutes.")
    }
}

/// Whether a failure that outlasted its retries is one another model could
/// mend: an overload or server error, an account out of credit or quota, or
/// a model the gateway does not serve, or a rate limit longer than the
/// retries wait out, which another account does not share.
pub fn mends(failure: &Failure) -> bool {
    // Once the reply is on screen, another model's would follow it rather
    // than stand in its place.
    !failure.stands
        && matches!(
            failure.kind,
            FailureKind::Overloaded
                | FailureKind::Server
                | FailureKind::Quota
                | FailureKind::RateLimit
                | FailureKind::NotFound
        )
}

/// Whether a fallback's failure is its own rather than the request's: its
/// gateway has no key, is not set up or cannot be reached, or the model reads
/// less than the chosen one. The next fallback may well answer.
fn own(failure: &Failure) -> bool {
    !failure.stands
        && matches!(
            failure.kind,
            FailureKind::Auth
                | FailureKind::Settings
                | FailureKind::Network
                | FailureKind::ContextTooLong
        )
}

/// The session's connection, then each of `fallbacks` on its own gateway,
/// leaving out any that repeat one before it.
pub fn chain(
    primary: &Connection,
    fallbacks: &[Fallback],
    keys: &HashMap<String, String>,
    custom: &str,
) -> Vec<Connection> {
    let mut chain = vec![primary.clone()];
    for fallback in fallbacks {
        let next = fallback.connect(primary, keys, custom);
        if !chain.iter().any(|c| same(c, &next)) {
            chain.push(next);
        }
    }
    chain
}

/// Send with the first of `chain` not cooling down, going on to the next
/// after a failure `mends` covers, or one of a fallback's `own`, which cools
/// that one down; a chosen model that was cooling is asked last. One that is
/// not `usable` is passed over quietly. `switched` hears of each switch
/// before the model is asked; an error from it ends the call, as nobody is
/// listening any more.
pub fn run(
    chain: &[Connection],
    mut usable: impl FnMut(&Connection) -> bool,
    mut send: impl FnMut(&Connection) -> Result<(), Failure>,
    mut switched: impl FnMut(Switch) -> Result<(), Failure>,
) -> Result<(), Failure> {
    let Some(primary) = chain.first() else {
        return Err(Failure::new(FailureKind::Settings, "No model to ask."));
    };
    let (ready, resting): (Vec<&Connection>, Vec<&Connection>) =
        chain.iter().partition(|c| !cooling(c));
    // The chosen model, though cooling down, is asked once the rest have
    // failed too, or are all cooling: it may be back by now, and without it
    // the task would end on a fallback's failure while it was never asked.
    let order = (ready.into_iter()).chain(resting.into_iter().filter(|c| same(c, primary)));
    let mut after: Option<(&Connection, FailureKind)> = None;
    let mut last = None;
    for connection in order {
        if !same(connection, primary) && !usable(connection) {
            continue;
        }
        // Going back to the chosen model after a fallback failed is a
        // switch too, or its reply would carry the fallback's name.
        if after.is_some() || !same(connection, primary) {
            // The same model on another account is told by the account.
            let from = after.map_or(primary, |(from, _)| from);
            let by_account = from.model.trim() == connection.model.trim();
            let label = |c: &Connection| {
                let model = c.model.trim().to_owned();
                match crate::gateway::named(&c.endpoint) {
                    Some(name) if by_account => name.to_owned(),
                    _ => model,
                }
            };
            switched(Switch {
                model: connection.model.trim().to_owned(),
                label: label(connection),
                endpoint: connection.endpoint.clone(),
                after: after.take().map(|(from, kind)| (label(from), kind)),
            })?;
        }
        let failure = match send(connection) {
            Ok(()) => return Ok(()),
            Err(failure) if mends(&failure) => failure,
            // Else a fallback with no key, or one not running, ends the task
            // and, with the chosen model cooling, every request after it.
            Err(failure) if !same(connection, primary) && own(&failure) => failure,
            Err(failure) => return Err(failure),
        };
        cool(connection, Instant::now() + COOLDOWN);
        after = Some((connection, failure.kind));
        last = Some(failure);
    }
    Err(last.unwrap_or_else(|| Failure::new(FailureKind::Unknown, "No model answered.")))
}

/// Whether two connections ask the same model at the same place.
pub(crate) fn same(a: &Connection, b: &Connection) -> bool {
    key(a) == key(b)
}

/// What a model is cooled down by: its gateway, told by its address, and its id.
fn key(connection: &Connection) -> (String, String) {
    (
        connection.endpoint.trim().trim_end_matches('/').to_owned(),
        connection.model.trim().to_owned(),
    )
}

/// Pass `connection`'s model over until `until`.
fn cool(connection: &Connection, until: Instant) {
    let mut cooling = COOLING.lock().unwrap_or_else(PoisonError::into_inner);
    cooling.insert(key(connection), until);
}

/// Whether `connection`'s model failed recently enough to be passed over.
fn cooling(connection: &Connection) -> bool {
    let mut cooling = COOLING.lock().unwrap_or_else(PoisonError::into_inner);
    let now = Instant::now();
    cooling.retain(|_, until| *until > now);
    cooling.contains_key(&key(connection))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    /// A connection to `model` at an address no other test uses, since the
    /// cooldowns are one table for the whole process.
    fn at(test: &str, model: &str) -> Connection {
        Connection {
            endpoint: format!("https://{test}.fallback.test/v1"),
            model: model.into(),
            key: "sk-session".into(),
            ..Default::default()
        }
    }

    /// Run `chain`, each model answering as `answer` says, and give back the
    /// models asked in turn and the switches told of.
    fn asked(
        chain: &[Connection],
        answer: impl Fn(&str) -> Result<(), Failure>,
    ) -> (Result<(), Failure>, Vec<String>, Vec<Switch>) {
        let asked = RefCell::new(Vec::new());
        let mut switches = Vec::new();
        let result = run(
            chain,
            |_| true,
            |c| {
                asked.borrow_mut().push(c.model.clone());
                answer(&c.model)
            },
            |switch| {
                switches.push(switch);
                Ok(())
            },
        );
        (result, asked.into_inner(), switches)
    }

    fn fails(kind: FailureKind) -> Result<(), Failure> {
        Err(Failure::new(kind, "It failed."))
    }

    #[test]
    fn an_overload_falls_through_to_the_next_model_and_says_so() {
        let chain = [at("overload", "big"), at("overload", "small")];
        let (result, asked, switches) = asked(&chain, |model| match model {
            "big" => fails(FailureKind::Overloaded),
            _ => Ok(()),
        });
        assert!(result.is_ok());
        assert_eq!(asked, ["big", "small"]);
        assert_eq!(
            switches[0].to_string(),
            "Switched to small: big is overloaded. It is asked again in 5 minutes."
        );
    }

    #[test]
    fn a_bad_request_a_stop_or_a_refusal_does_not_fall_through() {
        for (test, kind) in [
            ("bad-request", FailureKind::BadRequest),
            ("stopped", FailureKind::Stopped),
            ("refused", FailureKind::BadReply),
        ] {
            let chain = [at(test, "big"), at(test, "small")];
            let (result, asked, switches) = asked(&chain, |_| fails(kind));
            assert_eq!(result.unwrap_err().kind, kind);
            assert_eq!(asked, ["big"], "{kind:?}");
            assert!(switches.is_empty());
            // Nor is the model it was sent to cooled down.
            assert!(!cooling(&chain[0]));
        }
    }

    #[test]
    fn a_reply_already_begun_is_not_asked_again_elsewhere() {
        let chain = [at("begun", "big"), at("begun", "small")];
        let (result, asked, _) = asked(&chain, |_| {
            let mut failure = Failure::new(FailureKind::Server, "Broke off.");
            failure.stands = true;
            Err(failure)
        });
        assert!(result.is_err());
        assert_eq!(asked, ["big"]);
    }

    #[test]
    fn a_missing_model_or_an_empty_account_falls_through_too() {
        for (test, kind) in [
            ("missing", FailureKind::NotFound),
            ("quota", FailureKind::Quota),
            ("server", FailureKind::Server),
        ] {
            let chain = [at(test, "gone"), at(test, "here")];
            let (result, asked, switches) = asked(&chain, |model| match model {
                "gone" => fails(kind),
                _ => Ok(()),
            });
            assert!(result.is_ok(), "{kind:?}");
            assert_eq!(asked, ["gone", "here"]);
            assert_eq!(switches[0].after, Some(("gone".into(), kind)));
        }
    }

    #[test]
    fn a_fallback_that_fails_on_its_own_account_passes_to_the_next() {
        for (test, kind) in [
            ("no-key", FailureKind::Auth),
            ("not-running", FailureKind::Network),
            ("too-small", FailureKind::ContextTooLong),
        ] {
            let chain = [at(test, "big"), at(test, "broken"), at(test, "small")];
            let (result, asked, switches) = asked(&chain, |model| match model {
                "big" => fails(FailureKind::Overloaded),
                "broken" => fails(kind),
                _ => Ok(()),
            });
            assert!(result.is_ok(), "{kind:?}");
            assert_eq!(asked, ["big", "broken", "small"]);
            assert_eq!(switches[1].after, Some(("broken".into(), kind)));
            // It is passed over from then on, as a model that failed is.
            assert!(cooling(&chain[1]));
        }
        // The chosen model's own key or settings are the request's, and go no further.
        let chain = [at("primary-key", "big"), at("primary-key", "small")];
        let (result, asked, _) = asked(&chain, |_| fails(FailureKind::Auth));
        assert_eq!(result.unwrap_err().kind, FailureKind::Auth);
        assert_eq!(asked, ["big"]);
    }

    #[test]
    fn fallbacks_are_asked_in_order_and_the_last_failure_is_kept() {
        let chain = [at("order", "a"), at("order", "b"), at("order", "c")];
        let (result, asked, switches) = asked(&chain, |model| match model {
            "c" => fails(FailureKind::Quota),
            _ => fails(FailureKind::Overloaded),
        });
        assert_eq!(asked, ["a", "b", "c"]);
        assert_eq!(result.unwrap_err().kind, FailureKind::Quota);
        let told: Vec<_> = switches.iter().map(|s| s.model.as_str()).collect();
        assert_eq!(told, ["b", "c"]);
        assert_eq!(
            switches[1].after,
            Some(("b".into(), FailureKind::Overloaded))
        );
    }

    #[test]
    fn a_failed_model_is_passed_over_until_its_cooldown_ends() {
        let chain = [at("cooldown", "big"), at("cooldown", "small")];
        let (_, first, _) = asked(&chain, |model| match model {
            "big" => fails(FailureKind::Overloaded),
            _ => Ok(()),
        });
        assert_eq!(first, ["big", "small"]);
        // The next request goes straight to the fallback, quietly.
        let (result, second, switches) = asked(&chain, |_| Ok(()));
        assert!(result.is_ok());
        assert_eq!(second, ["small"]);
        assert_eq!(switches[0].after, None);
        assert_eq!(switches[0].to_string(), "Switched to small");
        // Once the cooldown is over the chosen model is asked first again.
        cool(&chain[0], Instant::now());
        let (_, third, switches) = asked(&chain, |_| Ok(()));
        assert_eq!(third, ["big"]);
        assert!(switches.is_empty());
    }

    #[test]
    fn with_every_model_cooling_the_chosen_one_is_tried() {
        let chain = [at("all-cool", "big"), at("all-cool", "small")];
        for connection in &chain {
            cool(connection, Instant::now() + COOLDOWN);
        }
        let (result, asked, _) = asked(&chain, |_| Ok(()));
        assert!(result.is_ok());
        assert_eq!(asked, ["big"]);
    }

    #[test]
    fn a_cooling_chosen_model_is_asked_once_its_fallbacks_fail() {
        let chain = [at("rested", "big"), at("rested", "small")];
        cool(&chain[0], Instant::now() + COOLDOWN);
        let (result, asked, switches) = asked(&chain, |model| match model {
            "small" => fails(FailureKind::Overloaded),
            _ => Ok(()),
        });
        assert!(result.is_ok());
        assert_eq!(asked, ["small", "big"]);
        let told: Vec<_> = switches.iter().map(|s| s.model.as_str()).collect();
        assert_eq!(told, ["small", "big"]);
        assert_eq!(
            switches[1].after,
            Some(("small".into(), FailureKind::Overloaded))
        );
    }

    #[test]
    fn a_fallback_on_another_gateway_takes_that_gateways_address_and_key() {
        let primary = at("gateways", "big");
        let fallbacks = Fallback::parse_all(&[
            "OpenRouter:deepseek/deepseek-chat".into(),
            "small".into(),
            "big".into(),
        ])
        .unwrap();
        let keys = HashMap::from([("OpenRouter".to_owned(), "sk-or".to_owned())]);
        let chain = chain(&primary, &fallbacks, &keys, "");
        let got: Vec<_> = chain
            .iter()
            .map(|c| (c.endpoint.as_str(), c.model.as_str(), c.key.as_str()))
            .collect();
        assert_eq!(
            got,
            [
                ("https://gateways.fallback.test/v1", "big", "sk-session"),
                (
                    "https://openrouter.ai/api/v1",
                    "deepseek/deepseek-chat",
                    "sk-or"
                ),
                ("https://gateways.fallback.test/v1", "small", "sk-session"),
            ]
        );
    }

    #[test]
    fn a_fallback_is_written_as_a_model_or_gateway_and_model() {
        let parsed = Fallback::parse(" openrouter:deepseek/deepseek-r1:free ").unwrap();
        assert_eq!(parsed.gateway, "OpenRouter");
        assert_eq!(parsed.model, "deepseek/deepseek-r1:free");
        assert_eq!(parsed.to_string(), "OpenRouter:deepseek/deepseek-r1:free");
        // A tag after a colon is part of the id when no gateway comes before it.
        let tagged = Fallback::parse("llama3:8b").unwrap();
        assert_eq!(
            (tagged.gateway.as_str(), tagged.model.as_str()),
            ("", "llama3:8b")
        );
        assert!(Fallback::parse("OpenAI:").is_err());
        assert!(Fallback::parse("  ").is_err());
    }
}
