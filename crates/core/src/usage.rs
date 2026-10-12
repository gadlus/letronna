//! What model calls took, in tokens and, where it can be known, dollars,
//! added up per session, day and model for `/usage`.
//!
//! Nothing is stored for this on its own. A reply's usage rides on its
//! message, which is saved anyway, and so does that of the summary made for
//! its request, so the totals are added up when asked by reading the session
//! files back, and nothing about a session outlives its file.
//!
//! A cost is what OpenRouter billed, an estimate, or not known. OpenRouter
//! says what a call cost when the request asks, and only OpenRouter is
//! asked: OpenAI turns away a request with a field it does not know.
//! Elsewhere the cost is estimated from the price the gateway's model list
//! gives. With neither it is left unknown rather than shown as nothing.

use crate::cache_miss::Reason;
use crate::chat::Message;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Days listed one by one: a week shows how use goes from day to day.
pub const DAYS: u64 = 7;
/// Days the per-model totals cover: about a month, the span of a bill.
pub const MODEL_DAYS: u64 = 30;

/// What one model call took.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Usage {
    /// All the input, cache hits and writes included.
    #[serde(default)]
    pub input: u64,
    /// Of the input, what the provider served from its prompt cache.
    #[serde(default)]
    pub cached: u64,
    /// Of the input, what the provider wrote to its prompt cache.
    #[serde(default)]
    pub cache_write: u64,
    #[serde(default)]
    pub output: u64,
    /// Of the output, what the model spent thinking.
    #[serde(default)]
    pub reasoning: u64,
    /// In US dollars, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost: Option<f64>,
    /// The cost comes from the listed price rather than a bill.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub estimated: bool,
    /// Why the cache served far less of the call than of the one before.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub miss: Option<Reason>,
}

impl Usage {
    /// What the call that made `reply` took, leaving out any summary.
    pub fn of(reply: &Message) -> Self {
        Self {
            input: reply.input_tokens(),
            cached: reply.cached_tokens,
            cache_write: reply.cache_write_tokens,
            output: reply.tokens,
            reasoning: reply.reasoning_tokens,
            cost: reply.cost,
            estimated: reply.cost_estimated,
            miss: reply.cache_miss,
        }
    }

    /// The usage in a chat completion's `usage` object, with the cost as
    /// billed, if it was.
    pub(crate) fn read(usage: &Value) -> Self {
        let count = |key: &str| usage[key].as_u64().unwrap_or(0);
        Self::of(&Message {
            tokens: count("completion_tokens"),
            prompt_tokens: count("prompt_tokens"),
            cached_tokens: cached(usage).unwrap_or(0),
            cache_write_tokens: cache_write(usage).unwrap_or(0),
            reasoning_tokens: reasoning(usage).unwrap_or(0),
            cost: billed(usage),
            ..Default::default()
        })
    }

    /// This usage of `model` at `endpoint` with its cost settled: kept when
    /// OpenRouter billed it, otherwise estimated from the listed price, and
    /// not known when neither is there or nothing was counted.
    pub(crate) fn priced(mut self, endpoint: &str, model: &str) -> Self {
        if self.cost.is_some() && asks_cost(endpoint) {
            self.estimated = false;
            return self;
        }
        let counted = self.input > 0 || self.output > 0;
        self.cost = crate::models::facts(endpoint, model.trim())
            .price
            .filter(|_| counted)
            .map(|price| estimate(&price, &self));
        self.estimated = self.cost.is_some();
        self
    }
}

/// Whether requests to `endpoint` ask for the cost: OpenRouter's alone.
pub(crate) fn asks_cost(endpoint: &str) -> bool {
    crate::chat::host(endpoint).is_some_and(|h| h == "openrouter.ai")
}

/// Input tokens the provider served from its prompt cache. OpenAI reports
/// them under `prompt_tokens_details`; DeepSeek and Anthropic use fields of
/// their own, which gateways pass through.
pub(crate) fn cached(usage: &Value) -> Option<u64> {
    usage
        .pointer("/prompt_tokens_details/cached_tokens")
        .or_else(|| usage.get("prompt_cache_hit_tokens"))
        .or_else(|| usage.get("cache_read_input_tokens"))
        .and_then(Value::as_u64)
}

/// Input tokens written to the prompt cache, under OpenRouter's name, the
/// one other gateways give beside `cached_tokens`, or Anthropic's. A name
/// given as null, as the Responses API's usage can, is passed over.
pub(crate) fn cache_write(usage: &Value) -> Option<u64> {
    [
        usage.pointer("/prompt_tokens_details/cache_write_tokens"),
        usage.pointer("/prompt_tokens_details/cache_creation_tokens"),
        usage.get("cache_creation_input_tokens"),
    ]
    .into_iter()
    .find_map(|tokens| tokens?.as_u64())
}

/// Output tokens the model spent thinking.
pub(crate) fn reasoning(usage: &Value) -> Option<u64> {
    usage
        .pointer("/completion_tokens_details/reasoning_tokens")
        .and_then(Value::as_u64)
}

/// What OpenRouter billed, in dollars. On a key of the user's own it bills
/// only its fee, and the provider bills the rest to that key.
pub(crate) fn billed(usage: &Value) -> Option<f64> {
    let fee = usage["cost"].as_f64()?;
    let upstream = usage["is_byok"]
        .as_bool()
        .filter(|&own| own)
        .and_then(|_| usage.pointer("/cost_details/upstream_inference_cost"))
        .and_then(Value::as_f64)
        .unwrap_or(0.);
    Some(fee + upstream).filter(|cost| cost.is_finite() && *cost >= 0.)
}

/// `usage` at `price`. Hits and writes the list gives no price for are
/// priced as fresh input, and thinking as output, which it is billed as.
fn estimate(price: &crate::models::Price, usage: &Usage) -> f64 {
    let fresh = usage
        .input
        .saturating_sub(usage.cached.saturating_add(usage.cache_write));
    fresh as f64 * price.input
        + usage.cached as f64 * price.cache_read.unwrap_or(price.input)
        + usage.cache_write as f64 * price.cache_write.unwrap_or(price.input)
        + usage.output as f64 * price.output
}

/// Usage added up over many calls.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Totals {
    pub calls: u64,
    pub input: u64,
    pub cached: u64,
    pub cache_write: u64,
    pub output: u64,
    pub reasoning: u64,
    /// In US dollars, over the calls whose cost is known.
    pub cost: f64,
    /// Calls whose cost is not known, which `cost` leaves out.
    pub unpriced: u64,
    /// Some of `cost` is estimated rather than billed.
    pub estimated: bool,
}

impl Totals {
    pub fn add(&mut self, usage: &Usage) {
        // A gateway can send any number, and one absurd count should not
        // wrap the rest round.
        self.calls += 1;
        self.input = self.input.saturating_add(usage.input);
        self.cached = self.cached.saturating_add(usage.cached);
        self.cache_write = self.cache_write.saturating_add(usage.cache_write);
        self.output = self.output.saturating_add(usage.output);
        self.reasoning = self.reasoning.saturating_add(usage.reasoning);
        match usage.cost {
            Some(cost) => {
                self.cost += cost;
                self.estimated |= usage.estimated;
            }
            None => self.unpriced += 1,
        }
    }

    /// The share of the input served from the prompt cache, in percent.
    pub fn cache_rate(&self) -> Option<u64> {
        let share = |input: u64| u128::from(self.cached.min(input)) * 100 / u128::from(input);
        (self.input > 0).then(|| share(self.input) as u64)
    }

    /// The cost as it reads: `~` before an estimate, `+` after a sum that
    /// leaves calls out, and `None` when no call's cost is known.
    pub fn cost_text(&self) -> Option<String> {
        if self.unpriced == self.calls {
            return None;
        }
        let about = if self.estimated { "~" } else { "" };
        let more = if self.unpriced > 0 { "+" } else { "" };
        // A short session costs a fraction of a cent, which two places
        // would show as nothing at all.
        let dollars = if self.cost > 0. && self.cost < 0.01 {
            format!("${:.4}", self.cost)
        } else {
            format!("${:.2}", self.cost)
        };
        Some(format!("{about}{dollars}{more}"))
    }

    /// The totals on one line: calls, input and the share the cache served,
    /// output and the thinking in it, and the cost where it is known.
    pub fn line(&self) -> String {
        if self.calls == 0 {
            return "nothing yet".to_owned();
        }
        let calls = if self.calls == 1 { "call" } else { "calls" };
        let mut parts = vec![
            format!("{} {calls}", self.calls),
            format!("{} in", count(self.input)),
        ];
        // Many routes never report hits, so none reads as not said.
        if let Some(rate) = self.cache_rate().filter(|&rate| rate > 0) {
            parts.push(format!("{rate}% cached"));
        }
        parts.push(format!("{} out", count(self.output)));
        if self.reasoning > 0 {
            parts.push(format!("{} thinking", count(self.reasoning)));
        }
        parts.extend(self.cost_text());
        parts.join(" · ")
    }
}

/// A token count as it reads at a glance: `950`, `12.3k`, `4.1M`.
pub fn count(tokens: u64) -> String {
    // Where a tenth of a thousand rounds up to the next unit, the next unit
    // reads it, or 999,960 would be `1000.0k`.
    match tokens {
        0..1_000 => tokens.to_string(),
        1_000..999_950 => format!("{:.1}k", tokens as f64 / 1e3),
        _ => format!("{:.1}M", tokens as f64 / 1e6),
    }
}

/// One saved model call, with where and when it was made.
#[derive(Clone, Debug, PartialEq)]
pub struct Spent {
    pub session: u64,
    /// Seconds since 1970.
    pub at: u64,
    pub model: String,
    pub usage: Usage,
    /// A reply a branch copied from an older session: its session counts
    /// it, but its day and model already have.
    pub copy: bool,
}

/// The calls behind `reply`, each with the model it was made on: its own,
/// and the summary made for its request when one was, which the side model
/// may have made.
pub fn calls(reply: &Message) -> impl Iterator<Item = (&str, Usage)> {
    let own = (reply.role == "assistant").then(|| (reply.model.as_str(), Usage::of(reply)));
    let summarizer = match reply.summary_model.as_str() {
        "" => reply.model.as_str(),
        model => model,
    };
    let summary = reply.summary_usage.map(|usage| (summarizer, usage));
    own.into_iter().chain(summary)
}

/// The totals for one session, the last `DAYS` days and the models of the
/// last `MODEL_DAYS` days.
#[derive(Debug, Default, PartialEq)]
pub struct Report {
    pub session: Totals,
    /// Each day with any use, newest first.
    pub days: Vec<(chrono::NaiveDate, Totals)>,
    /// Each model used, the most tokens first.
    pub models: Vec<(String, Totals)>,
    /// The session's latest cache misses, newest first.
    pub misses: Vec<Missed>,
}

/// A call the prompt cache served far less of than the one before it.
#[derive(Clone, Debug, PartialEq)]
pub struct Missed {
    /// Seconds since 1970.
    pub at: u64,
    pub model: String,
    pub reason: Reason,
}

impl Missed {
    /// The miss on one line: when, why, and on which model.
    pub fn line(&self) -> String {
        let at = i64::try_from(self.at).ok();
        let local =
            at.and_then(|at| chrono::TimeZone::timestamp_opt(&chrono::Local, at, 0).single());
        let when = local.map_or_else(
            || "some time".to_owned(),
            |time| format!("{} {}", day_name(time.date_naive()), time.format("%H:%M")),
        );
        let model = self.model.trim();
        let model = if model.is_empty() {
            "unknown model"
        } else {
            model
        };
        format!("{when} · {} · {model}", self.reason.text())
    }
}

impl Report {
    /// What the marks on the costs mean, when any of them is there.
    pub fn legend(&self) -> Option<String> {
        let all = || {
            let days = self.days.iter().map(|(_, totals)| totals);
            let models = self.models.iter().map(|(_, totals)| totals);
            std::iter::once(&self.session).chain(days).chain(models)
        };
        let mut notes = Vec::new();
        if all().any(|totals| totals.estimated) {
            notes.push("~ estimated from the listed price");
        }
        if all().any(|totals| totals.unpriced > 0 && totals.unpriced < totals.calls) {
            notes.push("+ leaves out calls with no known price");
        }
        (!notes.is_empty()).then(|| notes.join(" · "))
    }
}

/// A day as a list names it: today, yesterday, or its weekday and date.
pub fn day_name(date: chrono::NaiveDate) -> String {
    match (chrono::Local::now().date_naive() - date).num_days() {
        0 => "today".to_owned(),
        1 => "yesterday".to_owned(),
        _ => date.format("%a %-d %b").to_string(),
    }
}

/// Add up the saved calls for `session` and for the days and models up to
/// now, on the local calendar.
pub fn report(session: u64) -> Report {
    let now = crate::chat::now();
    let spent = crate::store::spent(now.saturating_sub(MODEL_DAYS * 86_400), session);
    report_in(&spent, session, now, &chrono::Local)
}

/// `report` over `spent`, with days as `zone` counts them.
fn report_in<Tz: chrono::TimeZone>(spent: &[Spent], session: u64, now: u64, zone: &Tz) -> Report {
    let day = |at: u64| {
        zone.timestamp_opt(i64::try_from(at).ok()?, 0)
            .single()
            .map(|time| time.date_naive())
    };
    let today = day(now).unwrap_or_default();
    let first_day = today - chrono::Days::new(DAYS - 1);
    let model_since = now.saturating_sub(MODEL_DAYS * 86_400);
    let mut report = Report::default();
    let mut days = std::collections::BTreeMap::<chrono::NaiveDate, Totals>::new();
    let mut models = std::collections::HashMap::<&str, Totals>::new();
    for call in spent {
        if call.session == session {
            report.session.add(&call.usage);
            if let Some(reason) = call.usage.miss {
                report.misses.push(Missed {
                    at: call.at,
                    model: call.model.clone(),
                    reason,
                });
            }
        }
        if call.copy {
            continue;
        }
        if let Some(date) = day(call.at).filter(|date| (first_day..=today).contains(date)) {
            days.entry(date).or_default().add(&call.usage);
        }
        if call.at >= model_since {
            // Old replies can be saved without the name of their model.
            let model = Some(call.model.trim()).filter(|model| !model.is_empty());
            let model = model.unwrap_or("unknown model");
            models.entry(model).or_default().add(&call.usage);
        }
    }
    // A helper's transcript is read after its session's, so the file order
    // is not the order they happened in.
    report
        .misses
        .sort_by_key(|missed| std::cmp::Reverse(missed.at));
    report.misses.truncate(crate::cache_miss::SHOWN);
    report.days = days.into_iter().rev().collect();
    report.models = models
        .into_iter()
        .map(|(model, totals)| (model.to_owned(), totals))
        .collect();
    report.models.sort_by_key(|(model, totals)| {
        (
            std::cmp::Reverse(totals.input.saturating_add(totals.output)),
            model.clone(),
        )
    });
    report
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use serde_json::json;

    #[test]
    fn openrouter_usage_reads_its_cache_writes_thinking_and_bill() {
        let usage = Usage::read(&json!({
            "prompt_tokens": 5_000,
            "completion_tokens": 400,
            "prompt_tokens_details": {"cached_tokens": 4_000, "cache_write_tokens": 600},
            "completion_tokens_details": {"reasoning_tokens": 150},
            "cost": 0.0123
        }));
        assert_eq!(
            usage,
            Usage {
                input: 5_000,
                cached: 4_000,
                cache_write: 600,
                output: 400,
                reasoning: 150,
                cost: Some(0.0123),
                estimated: false,
                miss: None,
            }
        );
        // On the user's own key OpenRouter bills its fee, the provider the rest.
        let own_key = json!({"cost": 0.001, "is_byok": true,
            "cost_details": {"upstream_inference_cost": 0.02}});
        assert_eq!(billed(&own_key), Some(0.021));
    }

    #[test]
    fn anthropic_names_for_the_cache_are_read_too() {
        let usage = Usage::read(&json!({
            "prompt_tokens": 20,
            "completion_tokens": 9,
            "cache_read_input_tokens": 4_100,
            "cache_creation_input_tokens": 300
        }));
        // Counted beside the prompt rather than in it, so they add to it.
        assert_eq!(
            (usage.input, usage.cached, usage.cache_write),
            (4_420, 4_100, 300)
        );
        assert_eq!(usage.cost, None);
    }

    #[test]
    fn a_cache_write_is_read_under_each_name_a_gateway_uses() {
        let written = |usage: Value| cache_write(&usage);
        let openrouter = json!({"prompt_tokens_details": {"cache_write_tokens": 600}});
        assert_eq!(written(openrouter), Some(600));
        let gateway = json!({"prompt_tokens_details": {"cache_creation_tokens": 18_012}});
        assert_eq!(written(gateway), Some(18_012));
        let anthropic = json!({"cache_creation_input_tokens": 300});
        assert_eq!(written(anthropic), Some(300));
        // A name left null, as the Responses API's usage leaves it, is no count.
        let null = json!({"prompt_tokens_details": {"cache_write_tokens": null},
            "cache_creation_input_tokens": 300});
        assert_eq!(written(null), Some(300));
        assert_eq!(written(json!({"prompt_tokens": 20})), None);
    }

    #[test]
    fn a_cost_is_estimated_only_where_a_price_is_listed() {
        let endpoint = "https://priced.test/v1";
        crate::models::remember(
            endpoint,
            &[json!({"id": "listed", "pricing": {
                "prompt": "0.000002", "completion": "0.00001", "input_cache_read": "0.0000002"
            }})],
        );
        let usage = Usage {
            input: 1_000_000,
            cached: 500_000,
            output: 100_000,
            ..Default::default()
        };
        let priced = usage.priced(endpoint, "listed");
        // 500k fresh at $2/M, 500k hits at $0.20/M and 100k out at $10/M.
        let cost = priced.cost.unwrap();
        assert!((cost - 2.1).abs() < 1e-9, "{cost}");
        assert!(priced.estimated);
        // Unlisted, or nothing counted: no price, so no cost.
        assert_eq!(usage.priced(endpoint, "unlisted").cost, None);
        assert_eq!(Usage::default().priced(endpoint, "listed").cost, None);
    }

    #[test]
    fn a_billed_cost_is_trusted_from_openrouter_alone() {
        let billed = Usage {
            input: 10,
            output: 10,
            cost: Some(0.5),
            ..Default::default()
        };
        let kept = billed.priced("https://openrouter.ai/api/v1", "some/model");
        assert_eq!((kept.cost, kept.estimated), (Some(0.5), false));
        // Another gateway's `cost` may be in its own credits, not dollars.
        let elsewhere = billed.priced("https://other.test/v1", "some/model");
        assert_eq!(elsewhere.cost, None);
    }

    #[test]
    fn the_cost_reads_as_estimated_or_partial_or_not_at_all() {
        let mut totals = Totals::default();
        let unpriced = Usage {
            input: 10,
            ..Default::default()
        };
        totals.add(&unpriced);
        assert_eq!(totals.cost_text(), None);
        totals.add(&Usage {
            cost: Some(0.42),
            estimated: true,
            ..unpriced
        });
        assert_eq!(totals.cost_text().as_deref(), Some("~$0.42+"));
        let billed = Totals {
            calls: 1,
            cost: 0.004,
            ..Default::default()
        };
        assert_eq!(billed.cost_text().as_deref(), Some("$0.0040"));
    }

    #[test]
    fn totals_add_up_by_session_day_and_model() {
        let day = 86_400;
        let noon = chrono::Utc.with_ymd_and_hms(2026, 9, 29, 12, 0, 0).unwrap();
        let now = noon.timestamp() as u64;
        let call = |session, at, model: &str, input, cached, cost| Spent {
            session,
            at,
            model: model.into(),
            usage: Usage {
                input,
                cached,
                output: 10,
                cost,
                ..Default::default()
            },
            copy: false,
        };
        let spent = [
            call(1, now, "a", 1_000, 800, Some(0.5)),
            call(1, now - 2 * day, "b", 3_000, 0, None),
            call(2, now - day, "a", 1_000, 200, Some(0.25)),
            // Past the week but inside the month: counted for its model.
            call(2, now - 10 * day, "b", 5_000, 0, None),
            // Past the month: counted for its session alone.
            call(1, now - 40 * day, "c", 7, 0, None),
            // A branch's copy of the first: paid for once, on its own day.
            Spent {
                session: 3,
                copy: true,
                ..call(1, now, "a", 1_000, 800, Some(0.5))
            },
        ];
        let report = report_in(&spent, 1, now, &chrono::Utc);
        assert_eq!(report_in(&spent, 3, now, &chrono::Utc).session.calls, 1);

        assert_eq!((report.session.calls, report.session.input), (3, 4_007));
        let days: Vec<(String, u64)> = report
            .days
            .iter()
            .map(|(date, totals)| (date.to_string(), totals.calls))
            .collect();
        let newest_first = ["2026-09-29", "2026-09-28", "2026-09-27"].map(|d| (d.to_owned(), 1));
        assert_eq!(days, newest_first);
        let models: Vec<(&str, u64)> = report
            .models
            .iter()
            .map(|(model, totals)| (model.as_str(), totals.calls))
            .collect();
        assert_eq!(models, [("b", 2), ("a", 2)]);
        let a = &report.models[1].1;
        assert_eq!(a.cache_rate(), Some(50));
        assert_eq!(a.cost_text().as_deref(), Some("$0.75"));
    }

    #[test]
    fn the_sessions_latest_misses_are_listed_newest_first() {
        let missed = |session, at, reason| Spent {
            session,
            at,
            model: "m".into(),
            usage: Usage {
                miss: Some(reason),
                ..Default::default()
            },
            copy: false,
        };
        let mut spent: Vec<Spent> = (0..7)
            .map(|at| missed(1, 1_000 + at, Reason::Summary))
            .collect();
        // A helper's miss read after them, but the newest of all.
        spent.push(missed(1, 2_000, Reason::Tools));
        spent.push(missed(2, 3_000, Reason::Memory));
        let report = report_in(&spent, 1, 3_000, &chrono::Utc);
        let reasons: Vec<(u64, Reason)> = report
            .misses
            .iter()
            .map(|missed| (missed.at, missed.reason))
            .collect();
        assert_eq!(reasons.len(), crate::cache_miss::SHOWN);
        assert_eq!(reasons[0], (2_000, Reason::Tools));
        assert_eq!(reasons[1], (1_006, Reason::Summary));
        assert!(report.misses[0].line().contains("the tools changed · m"));
    }

    #[test]
    fn counts_read_at_a_glance() {
        assert_eq!(count(950), "950");
        assert_eq!(count(12_345), "12.3k");
        assert_eq!(count(999_960), "1.0M");
        assert_eq!(count(4_100_000), "4.1M");
    }

    #[test]
    fn summary_usage_is_attributed_to_the_side_model() {
        let line = r#"{"role":"assistant","content":"","model":"big-model","prompt_tokens":70,"tokens":7,"summary_usage":{"input":900,"output":60},"summary_model":"small-model"}"#;
        let reply: Message = serde_json::from_str(line).unwrap();
        let made: Vec<(&str, u64)> = calls(&reply)
            .map(|(model, usage)| (model, usage.input))
            .collect();
        assert_eq!(made, [("big-model", 70), ("small-model", 900)]);
        assert!(
            serde_json::to_string(&reply)
                .unwrap()
                .contains(r#""summary_model":"small-model""#)
        );
        // One saved before summaries named their model was made on the reply's.
        let older = Message {
            summary_model: String::new(),
            ..reply
        };
        let made: Vec<&str> = calls(&older).map(|(model, _)| model).collect();
        assert_eq!(made, ["big-model", "big-model"]);
        assert!(
            !serde_json::to_string(&older)
                .unwrap()
                .contains("summary_model")
        );
    }
}
