//! How the model has been used over the last few weeks, for `/insights`:
//! replies and tokens per day, the hours of the day they come in, the tools
//! and models used most, and how much of the input the cache served.
//!
//! Like `/usage`, nothing is kept for this apart from the sessions. It is
//! added up when asked by reading the session files back a line at a time,
//! passing over any file not written to since the weeks began, so the cost
//! follows how much was done lately rather than how much was ever done.

use crate::chat::Message;
use crate::usage::{Totals, calls};

/// Weeks looked back over when none are asked for: about a month.
pub const WEEKS: u64 = 4;
/// The most weeks looked back over, so a typo cannot read every file ever.
pub const MAX_WEEKS: u64 = 52;
/// Tools and models listed at most, the rest being a long tail of ones.
pub const TOP: usize = 8;

/// One day's replies and the tokens their calls took.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Day {
    pub date: chrono::NaiveDate,
    pub replies: u64,
    /// Input and output together, summaries made for the replies included.
    pub tokens: u64,
}

/// What the last `weeks` weeks came to.
#[derive(Debug, Default, PartialEq)]
pub struct Insights {
    pub weeks: u64,
    /// Every day of the weeks, oldest first, quiet ones included.
    pub days: Vec<Day>,
    /// Replies by the local hour they finished in, from midnight.
    pub hours: [u64; 24],
    /// Each tool the model called, with how often, the most first.
    pub tools: Vec<(String, u64)>,
    /// Each model used, with its totals, the most tokens first.
    pub models: Vec<(String, Totals)>,
    /// Every call of the weeks added up.
    pub total: Totals,
}

impl Insights {
    /// Replies over the weeks.
    pub fn replies(&self) -> u64 {
        self.days.iter().map(|day| day.replies).sum()
    }
}

/// The insights for the last `weeks` weeks up to now, on the local calendar.
pub fn insights(weeks: u64) -> Insights {
    let weeks = weeks.clamp(1, MAX_WEEKS);
    let now = crate::chat::now();
    let mut tally = Tally::new(weeks, now, &chrono::Local);
    crate::store::replies(tally.since, |at, reply| tally.add(at, reply));
    tally.finish()
}

/// The insights as they are added up, a reply at a time.
struct Tally<'a, Tz: chrono::TimeZone> {
    zone: &'a Tz,
    weeks: u64,
    first_day: chrono::NaiveDate,
    /// When the first day began, in seconds since 1970.
    since: u64,
    days: Vec<Day>,
    hours: [u64; 24],
    tools: std::collections::HashMap<String, u64>,
    models: std::collections::HashMap<String, Totals>,
    total: Totals,
}

impl<'a, Tz: chrono::TimeZone> Tally<'a, Tz> {
    /// Nothing yet, over the `weeks` weeks up to `now` as `zone` counts days.
    fn new(weeks: u64, now: u64, zone: &'a Tz) -> Self {
        let today = local(zone, now).map(|time| time.date_naive());
        let today = today.unwrap_or_default();
        let first_day = today - chrono::Days::new(weeks * 7 - 1);
        let days = first_day
            .iter_days()
            .take_while(|date| *date <= today)
            .map(|date| Day {
                date,
                ..Default::default()
            })
            .collect();
        // A day can be longer than 24 hours where the clocks change, so the
        // day's own midnight marks the start rather than a count of seconds.
        let since = first_day
            .and_hms_opt(0, 0, 0)
            .and_then(|midnight| midnight.and_local_timezone(zone.clone()).earliest())
            .and_then(|start| u64::try_from(start.timestamp()).ok())
            .unwrap_or_else(|| now.saturating_sub(weeks * 7 * 86_400));
        Self {
            zone,
            weeks,
            first_day,
            since,
            days,
            hours: [0; 24],
            tools: Default::default(),
            models: Default::default(),
            total: Totals::default(),
        }
    }

    /// Count `reply`, made at `at`, when that falls within the weeks.
    fn add(&mut self, at: u64, reply: &Message) {
        use chrono::Timelike;
        let Some(time) = local(self.zone, at) else {
            return;
        };
        let offset = (time.date_naive() - self.first_day).num_days();
        let Some(day) = usize::try_from(offset)
            .ok()
            .and_then(|offset| self.days.get_mut(offset))
        else {
            return;
        };
        day.replies += 1;
        self.hours[time.hour() as usize % 24] += 1;
        for (model, usage) in calls(reply) {
            day.tokens = day
                .tokens
                .saturating_add(usage.input.saturating_add(usage.output));
            // Old replies can be saved without the name of their model.
            let model = Some(model.trim()).filter(|model| !model.is_empty());
            let model = model.unwrap_or("unknown model");
            self.models.entry(model.to_owned()).or_default().add(&usage);
            self.total.add(&usage);
        }
        for call in &reply.tool_calls {
            *self.tools.entry(call.name.clone()).or_default() += 1;
        }
    }

    /// The insights, with the tools and models cut to the top few.
    fn finish(self) -> Insights {
        let mut tools: Vec<(String, u64)> = self.tools.into_iter().collect();
        tools.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        tools.truncate(TOP);
        let mut models: Vec<(String, Totals)> = self.models.into_iter().collect();
        models.sort_by_key(|(model, totals)| {
            (
                std::cmp::Reverse(totals.input.saturating_add(totals.output)),
                model.clone(),
            )
        });
        models.truncate(TOP);
        Insights {
            weeks: self.weeks,
            days: self.days,
            hours: self.hours,
            tools,
            models,
            total: self.total,
        }
    }
}

/// `at` as the clock in `zone` reads it.
fn local<Tz: chrono::TimeZone>(zone: &Tz, at: u64) -> Option<chrono::DateTime<Tz>> {
    zone.timestamp_opt(i64::try_from(at).ok()?, 0).single()
}

/// A bar of up to `width` blocks for `value` against `most`, in eighths so
/// small differences still show, and never empty for a value above nothing.
pub fn bar(value: u64, most: u64, width: usize) -> String {
    const EIGHTHS: [char; 8] = ['▏', '▎', '▍', '▌', '▋', '▊', '▉', '█'];
    if value == 0 || most == 0 || width == 0 {
        return String::new();
    }
    let eighths = u128::from(value.min(most)) * (width as u128) * 8 / u128::from(most);
    let eighths = (eighths as usize).max(1);
    let mut bar = "█".repeat(eighths / 8);
    if !eighths.is_multiple_of(8) {
        bar.push(EIGHTHS[eighths % 8 - 1]);
    }
    bar
}

/// `values` as one line of block heights, a character each, blank for
/// none, so a day's hours or a run of days read at a glance.
pub fn spark(values: &[u64]) -> String {
    const HEIGHTS: [char; 8] = ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];
    let most = values.iter().copied().max().unwrap_or(0);
    values
        .iter()
        .map(|&value| match value {
            0 => ' ',
            _ => HEIGHTS[(u128::from(value) * 7 / u128::from(most.max(1))) as usize % 8],
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chat::ToolCall;
    use chrono::TimeZone;

    fn reply(model: &str, input: u64, cached: u64, tools: &[&str]) -> Message {
        Message {
            role: "assistant".into(),
            model: model.into(),
            prompt_tokens: input,
            cached_tokens: cached,
            tokens: 10,
            tool_calls: tools
                .iter()
                .map(|name| ToolCall {
                    id: String::new(),
                    name: (*name).into(),
                    arguments: "{}".into(),
                })
                .collect(),
            ..Default::default()
        }
    }

    #[test]
    fn replies_add_up_by_day_hour_tool_and_model() {
        let noon = chrono::Utc.with_ymd_and_hms(2026, 9, 29, 12, 0, 0).unwrap();
        let now = noon.timestamp() as u64;
        let mut tally = Tally::new(1, now, &chrono::Utc);
        assert_eq!(tally.since, now - 6 * 86_400 - 12 * 3_600);
        tally.add(now, &reply("a", 1_000, 800, &["read", "grep"]));
        tally.add(now - 3_600, &reply("a", 1_000, 0, &["read"]));
        tally.add(now - 2 * 86_400, &reply("b", 4_000, 1_000, &[]));
        // Before the week began: left out.
        tally.add(tally.since - 1, &reply("c", 9_000, 0, &["run"]));
        let insights = tally.finish();

        assert_eq!(insights.days.len(), 7);
        assert_eq!(insights.days[0].date.to_string(), "2026-09-23");
        let today = insights.days[6];
        assert_eq!((today.replies, today.tokens), (2, 2_020));
        assert_eq!(insights.days[4].replies, 1);
        assert_eq!(insights.replies(), 3);
        assert_eq!((insights.hours[12], insights.hours[11]), (2, 1));
        assert_eq!(
            insights.tools,
            [("read".to_owned(), 2), ("grep".to_owned(), 1)]
        );
        let models: Vec<(&str, Option<u64>)> = insights
            .models
            .iter()
            .map(|(model, totals)| (model.as_str(), totals.cache_rate()))
            .collect();
        assert_eq!(models, [("b", Some(25)), ("a", Some(40))]);
        assert_eq!(insights.total.cache_rate(), Some(30));
    }

    #[test]
    fn a_summary_made_for_a_reply_counts_its_tokens_but_not_as_a_reply() {
        let now = 1_790_000_000;
        let mut tally = Tally::new(1, now, &chrono::Utc);
        let mut summarized = reply("a", 100, 0, &[]);
        summarized.summary_usage = Some(crate::usage::Usage {
            input: 900,
            output: 60,
            ..Default::default()
        });
        tally.add(now, &summarized);
        let insights = tally.finish();
        assert_eq!(insights.replies(), 1);
        assert_eq!(insights.days[6].tokens, 1_070);
        assert_eq!(insights.total.calls, 2);
    }

    #[test]
    fn bars_grow_in_eighths_and_never_vanish() {
        assert_eq!(bar(0, 10, 8), "");
        assert_eq!(bar(10, 10, 4), "████");
        assert_eq!(bar(5, 10, 4), "██");
        assert_eq!(bar(1, 1_000, 4), "▏");
        assert_eq!(bar(3, 16, 2), "▍");
    }

    #[test]
    fn a_spark_line_is_blank_for_none_and_full_for_the_most() {
        assert_eq!(spark(&[0, 1, 4, 8]), " ▁▄█");
        assert_eq!(spark(&[0, 0]), "  ");
    }

    #[test]
    fn scratch_sessions_are_read_back_and_old_files_passed_over() {
        let _turn = crate::store::tests::DATA_DIR
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let dir = std::env::temp_dir().join("letronna-insights-scratch");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("sessions")).unwrap();
        unsafe { std::env::set_var("AGENT_DATA_DIR", &dir) };
        let now = crate::chat::now();
        let recent = [
            r#"{"type":"head","id":1,"created":100}"#.to_owned(),
            r#"{"type":"message","role":"user","content":"hi"}"#.to_owned(),
            format!(
                r#"{{"type":"message","role":"assistant","content":"ok","model":"m","tokens":5,"prompt_tokens":50,"cached_tokens":20,"at":{now},"tool_calls":[{{"id":"1","name":"read","arguments":"{{}}"}}]}}"#
            ),
        ];
        std::fs::write(dir.join("sessions").join("1.jsonl"), recent.join("\n")).unwrap();
        // A branch holding a copy of the same reply: counted once.
        let branch = [r#"{"type":"head","id":2,"created":200}"#, &recent[2]];
        std::fs::write(dir.join("sessions").join("2.jsonl"), branch.join("\n")).unwrap();
        // Not written to for a year: never opened, whatever it holds.
        let old = dir.join("sessions").join("3.jsonl");
        std::fs::write(&old, recent[2].replace(r#""m""#, r#""old""#)).unwrap();
        let year_ago = std::time::SystemTime::now() - std::time::Duration::from_secs(365 * 86_400);
        let file = std::fs::File::options().write(true).open(&old).unwrap();
        file.set_modified(year_ago).unwrap();

        let insights = insights(WEEKS);
        unsafe { std::env::remove_var("AGENT_DATA_DIR") };
        assert_eq!(insights.days.len(), 28);
        assert_eq!(insights.replies(), 1);
        assert_eq!(insights.tools, [("read".to_owned(), 1)]);
        assert_eq!(insights.total.cache_rate(), Some(40));
    }
}
