//! The Usage page in Settings: what the model calls took, per session, day
//! and model, as `/usage` shows it in the terminal.
//!
//! The totals come from reading the saved sessions back, which takes a
//! moment with many of them, so they are added up off the UI thread each
//! time the page is opened and kept until the next time, never per frame.
//! Below them, the insights of `/insights` over its default weeks.

use crate::prelude::*;
use gpui_component::{chart::BarChart, progress::Progress};
use letronna_core::insights::{self, Insights};
use letronna_core::usage::{self, Report, Totals};

actions!(usage, [OpenUsage]);

impl Chat {
    /// Settings, open on this page, with the totals added up afresh.
    pub(crate) fn open_usage(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.settings_page.update(cx, |page, cx| {
            *page = "Usage";
            cx.notify();
        });
        if self.settings_open {
            self.load_usage(cx);
        } else {
            self.open_settings(window, cx);
        }
    }

    /// Add the totals up again for the page, off the UI thread.
    pub(crate) fn load_usage(&mut self, cx: &mut Context<Self>) {
        let session = self.store.conversations[self.store.active].id;
        let added = cx
            .background_executor()
            .spawn(async move { (usage::report(session), insights::insights(insights::WEEKS)) });
        cx.spawn(async move |this, cx| {
            let (report, insights) = added.await;
            let _ = this.update(cx, |this, cx| {
                this.usage_report = Some(report);
                this.usage_insights = Some(insights);
                cx.notify();
            });
        })
        .detach();
    }
}

/// The page: the heading, then this session, the recent days and the models,
/// then the insights, or a line saying the totals are still being added up.
pub(crate) fn usage_page(report: Option<&Report>, found: Option<&Insights>, cx: &App) -> Div {
    let muted = cx.theme().muted_foreground;
    let page = v_flex().gap_5().child(super::settings::heading(
        "Usage",
        "Tokens and cost of the model calls, added up from the saved sessions.",
        cx,
    ));
    let Some(report) = report else {
        return page.child(div().text_sm().text_color(muted).child("Adding up…"));
    };
    let days = report
        .days
        .iter()
        .map(|(date, totals)| (usage::day_name(*date), totals))
        .collect();
    let models = report
        .models
        .iter()
        .map(|(model, totals)| (model.clone(), totals))
        .collect();
    page.child(section(
        None,
        vec![("This session".into(), &report.session)],
        cx,
    ))
    .child(section(
        Some(format!("The last {} days", usage::DAYS)),
        days,
        cx,
    ))
    .child(section(
        Some(format!("By model, the last {} days", usage::MODEL_DAYS)),
        models,
        cx,
    ))
    .when(!report.misses.is_empty(), |page| {
        page.child(misses(&report.misses, cx))
    })
    .children(
        report
            .legend()
            .map(|legend| div().text_xs().text_color(muted).child(legend)),
    )
    .children(found.map(|found| insights_section(found, cx)))
}

/// The insights: a line of totals, replies by day and by hour as bars, and
/// the tools and models used most with a bar each against the first.
fn insights_section(found: &Insights, cx: &App) -> Div {
    let muted = cx.theme().muted_foreground;
    let tokens = |totals: &Totals| totals.input.saturating_add(totals.output);
    let replies = found.replies();
    let weeks = if found.weeks == 1 { "week" } else { "weeks" };
    let mut summary = vec![
        format!("The last {} {weeks}", found.weeks),
        format!(
            "{replies} {}",
            if replies == 1 { "reply" } else { "replies" }
        ),
        format!("{} tokens", usage::count(tokens(&found.total))),
    ];
    summary.extend(
        found
            .total
            .cache_rate()
            .map(|rate| format!("{rate}% of input cached")),
    );
    let page = v_flex()
        .gap_5()
        .child(super::settings::heading(
            "Insights",
            "Your recent weeks by day and hour, with the tools and models used most.",
            cx,
        ))
        .child(div().text_sm().text_color(muted).child(summary.join(" · ")));
    if replies == 0 {
        return page.child(div().text_sm().text_color(muted).child("Nothing yet."));
    }
    let days: Vec<(String, f64)> = found
        .days
        .iter()
        .map(|day| (day.date.format("%-d %b").to_string(), day.tokens as f64))
        .collect();
    let hours: Vec<(String, f64)> = found
        .hours
        .iter()
        .enumerate()
        .map(|(hour, &replies)| (hour.to_string(), replies as f64))
        .collect();
    let bar_color = cx.theme().progress_bar;
    let chart = |title: &str, data: Vec<(String, f64)>, every: usize| {
        v_flex().gap_1().child(subheading(title, cx)).child(
            div().h(px(140.)).child(
                BarChart::new(data)
                    // Both charts are built on this line, which is the id
                    // each would take otherwise.
                    .id(SharedString::from(title.to_owned()))
                    .band(|(label, _): &(String, f64)| label.clone())
                    .value(|(_, value): &(String, f64)| *value)
                    .fill(move |_: &(String, f64), _, _, _| bar_color)
                    .tick_margin(every)
                    // Still under the pointer, and drawn at once.
                    .interactive(false)
                    .appear(false),
            ),
        )
    };
    let most_calls = found.tools.first().map_or(0, |(_, calls)| *calls);
    let tools = found.tools.iter().map(|(name, calls)| {
        let times = if *calls == 1 { "call" } else { "calls" };
        (name.clone(), *calls, most_calls, format!("{calls} {times}"))
    });
    let most_tokens = found.models.first().map_or(0, |(_, totals)| tokens(totals));
    let models = found.models.iter().map(|(name, totals)| {
        let cached = totals
            .cache_rate()
            .map_or("cache not reported".to_owned(), |rate| {
                format!("{rate}% cached")
            });
        let note = format!("{} tokens · {cached}", usage::count(tokens(totals)));
        (name.clone(), tokens(totals), most_tokens, note)
    });
    page.child(chart("Tokens by day", days, 7))
        .child(chart("Replies by hour of the day", hours, 6))
        .when(!found.tools.is_empty(), |page| {
            page.child(ranked("Top tools", tools.collect(), cx))
        })
        .child(ranked("Top models", models.collect(), cx))
}

/// A small heading over one part of the page.
fn subheading(title: &str, cx: &App) -> Div {
    div()
        .text_xs()
        .font_family(HEADING_FONT)
        .text_color(cx.theme().muted_foreground)
        .child(title.to_owned())
}

/// Rows of a name, a bar of its count against the most, and a note.
fn ranked(title: &str, rows: Vec<(String, u64, u64, String)>, cx: &App) -> Div {
    let muted = cx.theme().muted_foreground;
    let list = rows
        .into_iter()
        .fold(v_flex(), |list, (name, value, most, note)| {
            let share = value as f32 * 100. / most.max(1) as f32;
            let bar = SharedString::from(format!("{title}-{name}"));
            list.child(
                h_flex()
                    .gap_3()
                    .py_2()
                    .border_b_1()
                    .border_color(cx.theme().border)
                    .child(
                        div()
                            .w(px(200.))
                            .flex_shrink_0()
                            .truncate()
                            .text_sm()
                            .font_weight(FontWeight::MEDIUM)
                            .child(name),
                    )
                    .child(
                        div()
                            .w(px(120.))
                            .flex_shrink_0()
                            .child(Progress::new(bar).value(share)),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_sm()
                            .text_color(muted)
                            .child(note),
                    ),
            )
        });
    v_flex().gap_1().child(subheading(title, cx)).child(list)
}

/// The session's latest cache misses, a line each with why, as `/usage`
/// lists them.
fn misses(misses: &[usage::Missed], cx: &App) -> Div {
    let muted = cx.theme().muted_foreground;
    v_flex()
        .gap_1()
        .child(
            div()
                .text_xs()
                .font_family(HEADING_FONT)
                .text_color(muted)
                .child("Cache misses in this session"),
        )
        .children(misses.iter().map(|missed| {
            div()
                .py_1()
                .text_sm()
                .text_color(muted)
                .child(missed.line())
        }))
}

/// A list of totals, a row each with hairlines between, under `title`.
fn section(title: Option<String>, rows: Vec<(String, &Totals)>, cx: &App) -> Div {
    let muted = cx.theme().muted_foreground;
    let none = rows.is_empty();
    let list = rows.into_iter().fold(v_flex(), |list, (name, totals)| {
        list.child(
            h_flex()
                .gap_3()
                .py_2()
                .border_b_1()
                .border_color(cx.theme().border)
                .child(
                    div()
                        .w(px(200.))
                        .flex_shrink_0()
                        .truncate()
                        .text_sm()
                        .font_weight(FontWeight::MEDIUM)
                        .child(name),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_sm()
                        .text_color(muted)
                        .child(totals.line()),
                ),
        )
    });
    v_flex()
        .gap_1()
        .children(title.map(|title| {
            div()
                .text_xs()
                .font_family(HEADING_FONT)
                .text_color(muted)
                .child(title)
        }))
        .child(list)
        .when(none, |this| {
            this.child(
                div()
                    .py_2()
                    .text_sm()
                    .text_color(muted)
                    .child("Nothing yet."),
            )
        })
}
