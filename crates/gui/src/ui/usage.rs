//! The Usage page in Settings: what the model calls took, per session, day
//! and model, as `/usage` shows it in the terminal.
//!
//! The totals come from reading the saved sessions back, which takes a
//! moment with many of them, so they are added up off the UI thread each
//! time the page is opened and kept until the next time, never per frame.

use crate::prelude::*;
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
            .spawn(async move { usage::report(session) });
        cx.spawn(async move |this, cx| {
            let report = added.await;
            let _ = this.update(cx, |this, cx| {
                this.usage_report = Some(report);
                cx.notify();
            });
        })
        .detach();
    }
}

/// The page: the heading, then this session, the recent days and the models,
/// or a line saying the totals are still being added up.
pub(crate) fn usage_page(report: Option<&Report>, cx: &App) -> Div {
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
