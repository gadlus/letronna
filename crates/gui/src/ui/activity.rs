//! The Activity page: the helpers this window's agents started, how each is
//! getting on, and a way to read, steer or stop one.
//!
//! Core keeps its helpers in a registry and their counts in atomics, so
//! nothing tells the window when one moves on. The window reads them four
//! times a second while a turn runs, since only a turn starts one, or while
//! one works, and not at all otherwise, so an idle window runs no timer. The
//! page, the status bar's pill and the sidebar's dots all draw from that last
//! read. Core knows when a run started but not when it ended, so a helper's
//! clock stops at the first read that finds it ended, and one never seen
//! working shows no time at all.

use crate::prelude::*;
use letronna_core::agents::{self, Seen};
use letronna_core::driver::AgentId;
use letronna_core::tools;
use std::sync::atomic::Ordering;
use std::time::Duration;

/// Four reads a second: a count a person can watch move, and a helper that
/// ends shows it within a quarter of a second.
const TICK: Duration = Duration::from_millis(250);

/// How near its bottom an open transcript still follows what the helper
/// adds: a line or two, as the chat does.
const FOLLOW: Pixels = px(48.);

/// Under the 4,000 characters core keeps of one notice, leaving room for the
/// line that says it is the user's and the indent each of its lines gets.
const MAX_STEER: usize = 3_500;

/// A helper as the window last read it.
pub(crate) struct Helper {
    pub(crate) seen: Seen,
    /// How long it has run, or ran, in seconds; None for one first read
    /// after it had ended, whose time is not known.
    pub(crate) seconds: Option<u64>,
}

/// A helper's transcript, open on the page.
pub(crate) struct Viewing {
    id: AgentId,
    session: u64,
    n: u32,
    path: PathBuf,
    /// The file's length when it was read, so it is read again only once it grows.
    length: u64,
    messages: Vec<Message>,
    scroll: ScrollHandle,
}

impl Chat {
    /// Starts a timer, when none runs and a helper could move, that reads
    /// the helpers every tick until none could.
    pub(crate) fn watch_helpers(&mut self, cx: &mut Context<Self>) {
        if self.helper_tick || !self.helpers_may_move() {
            return;
        }
        self.helper_tick = true;
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(TICK).await;
                let going = this.update(cx, |this, cx| {
                    let moved = this.read_helpers();
                    let reloaded = this.reload_transcript();
                    // Away from the page only the pill and the dots show
                    // helpers, and they change only as one starts or ends.
                    let ticking = this.page == Page::Activity && this.helpers_working() > 0;
                    if moved || reloaded || ticking {
                        cx.notify();
                    }
                    // Read once more after the turn ends, which catches a
                    // helper its last step started.
                    this.helper_tick = this.helpers_may_move();
                    this.helper_tick
                });
                if !matches!(going, Ok(true)) {
                    break;
                }
            }
        })
        .detach();
    }

    /// Reads the helpers again, and says whether one started or ended since
    /// the last read.
    fn read_helpers(&mut self) -> bool {
        let before = working(&self.helpers);
        self.helpers = refreshed(&self.helpers, agents::seen());
        working(&self.helpers) != before
    }

    /// Whether a helper could start or end: a turn is running, which is all
    /// that starts one, or one is working.
    fn helpers_may_move(&self) -> bool {
        self.busy || self.helpers_working() > 0
    }

    /// How many helpers are working now.
    pub(crate) fn helpers_working(&self) -> usize {
        working(&self.helpers).len()
    }

    /// Whether a helper is working for session `id`.
    pub(crate) fn helpers_working_in(&self, id: u64) -> bool {
        (self.helpers.iter()).any(|helper| helper.seen.session == id && helper.seen.ended.is_none())
    }

    /// The Activity page with its list showing, whatever transcript was open.
    pub(crate) fn open_activity(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.page = Page::Activity;
        self.viewing = None;
        self.focus_activity(window);
        cx.notify();
    }

    /// Gives the keyboard to the page, so Tab goes on through its buttons
    /// when the one it was on is gone.
    fn focus_activity(&self, window: &mut Window) {
        self.activity_focus.focus(window);
    }

    fn open_transcript(&mut self, id: AgentId, window: &mut Window, cx: &mut Context<Self>) {
        let Some(helper) = self.helpers.iter().find(|helper| helper.seen.id == id) else {
            return;
        };
        let (session, n) = (helper.seen.session, helper.seen.n);
        let Some(transcript) = Transcript::open(session, n) else {
            self.error = Some(format!(
                "{}'s conversation could not be read.",
                helper.seen.name
            ));
            cx.notify();
            return;
        };
        let length = std::fs::metadata(&transcript.path).map_or(0, |meta| meta.len());
        let scroll = ScrollHandle::new();
        scroll.scroll_to_bottom();
        self.viewing = Some(Viewing {
            id,
            session,
            n,
            path: transcript.path,
            length,
            messages: transcript.conversation.messages,
            scroll,
        });
        self.focus_activity(window);
        cx.notify();
    }

    /// Reads the open transcript again once the helper has added to it,
    /// staying at its end if that is where it was, and says whether it did.
    fn reload_transcript(&mut self) -> bool {
        if self.page != Page::Activity {
            return false;
        }
        let Some(viewing) = self.viewing.as_mut() else {
            return false;
        };
        let length = std::fs::metadata(&viewing.path).map_or(0, |meta| meta.len());
        if length == viewing.length {
            return false;
        }
        let Some(transcript) = Transcript::open(viewing.session, viewing.n) else {
            return false;
        };
        let scroll = &viewing.scroll;
        let at_end = scroll.max_offset().height + scroll.offset().y <= FOLLOW;
        viewing.messages = transcript.conversation.messages;
        viewing.length = length;
        if at_end {
            scroll.scroll_to_bottom();
        }
        true
    }

    fn open_steer(&mut self, id: AgentId, window: &mut Window, cx: &mut Context<Self>) {
        self.steering = Some(id);
        self.steer.update(cx, |input, cx| {
            input.set_value("", window, cx);
            input.focus(window, cx);
        });
        cx.notify();
    }

    /// Leaves what is in the steer box for the helper to read at its next
    /// step. The box stays, with the text, when the helper has ended.
    pub(crate) fn send_steer(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(id) = self.steering else {
            return;
        };
        let text = self.steer.read(cx).value().trim().to_owned();
        if text.is_empty() {
            return;
        }
        if text.chars().count() > MAX_STEER {
            // Core would cut its middle out, and the helper would never know.
            self.error = Some("Please keep a message to a helper under 3,500 characters.".into());
            cx.notify();
            return;
        }
        let name = self.helper_name(id);
        if !agents::steer(id, &text) {
            self.error = Some(format!(
                "{name} has ended, so it cannot read this. Ask in its session to pick it back up."
            ));
            cx.notify();
            return;
        }
        self.steer
            .update(cx, |input, cx| input.set_value("", window, cx));
        self.close_steer(window, cx);
        let sent = format!("Sent to {name}; it reads it at its next step.");
        window.push_notification(gpui_component::notification::Notification::info(sent), cx);
    }

    /// Closes the steer box, and gives the keyboard back to the page, since
    /// the box that had it is gone.
    pub(crate) fn close_steer(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.steering = None;
        self.focus_activity(window);
        cx.notify();
    }

    fn stop_helper(&mut self, id: AgentId, window: &mut Window, cx: &mut Context<Self>) {
        if !agents::stop(id) {
            self.error = Some(format!("{} is not running any more.", self.helper_name(id)));
        }
        if self.steering == Some(id) {
            self.steering = None;
        }
        // Its Steer and Stop go once it has ended.
        self.focus_activity(window);
        cx.notify();
    }

    /// Helper `id`'s name, as the last read had it.
    fn helper_name(&self, id: AgentId) -> String {
        (self.helpers.iter())
            .find(|helper| helper.seen.id == id)
            .map_or("The helper".to_owned(), |helper| helper.seen.name.clone())
    }

    pub(crate) fn activity_page(&self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        if let Some(viewing) = &self.viewing {
            return self.transcript_view(viewing, window, cx).into_any_element();
        }
        // Newest first: the one just started is the one being looked for.
        let rows: Vec<AnyElement> = (self.helpers.iter().rev())
            .map(|helper| self.helper_row(helper, cx).into_any_element())
            .collect();
        let list = if rows.is_empty() {
            div()
                .text_sm()
                .text_color(cx.theme().muted_foreground)
                .child(
                    "No helpers yet. When an agent hands part of its work to one, it shows here.",
                )
                .into_any_element()
        } else {
            div()
                .id("helper-list")
                .flex_1()
                .min_h_0()
                .overflow_y_scroll()
                .child(v_flex().gap_2().children(rows))
                .into_any_element()
        };
        v_flex()
            .track_focus(&self.activity_focus)
            .flex_1()
            .min_h_0()
            .px_8()
            .py_6()
            .gap_4()
            .child(
                v_flex()
                    .gap_1()
                    .child(
                        div()
                            .text_xl()
                            .font_family(HEADING_FONT)
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(Page::Activity.title()),
                    )
                    .child(
                        div()
                            .text_sm()
                            .text_color(cx.theme().muted_foreground)
                            .child(Page::Activity.blurb()),
                    ),
            )
            .child(list)
            .into_any_element()
    }

    fn helper_row(&self, helper: &Helper, cx: &mut Context<Self>) -> impl IntoElement {
        let seen = &helper.seen;
        let session = (self.store.index_of(seen.session))
            .map(|ix| self.store.conversations[ix].title.clone());
        v_flex()
            .w_full()
            .gap_2()
            .p_3()
            .rounded(cx.theme().radius)
            .bg(cx.theme().secondary.opacity(0.3))
            .child(
                h_flex()
                    .w_full()
                    .items_center()
                    .gap_3()
                    .child(state_dot(seen, cx))
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .gap_0p5()
                            .child(
                                h_flex()
                                    .min_w_0()
                                    .gap_2()
                                    .items_center()
                                    .child(
                                        div()
                                            .text_sm()
                                            .font_weight(FontWeight::MEDIUM)
                                            .child(seen.name.clone()),
                                    )
                                    .child(
                                        div()
                                            .min_w_0()
                                            .truncate()
                                            .text_xs()
                                            .text_color(cx.theme().muted_foreground)
                                            .child(match session {
                                                Some(title) => format!("{} · {title}", seen.kind),
                                                None => seen.kind.clone(),
                                            }),
                                    ),
                            )
                            .when(!seen.description.is_empty(), |this| {
                                this.child(
                                    div().text_xs().truncate().child(seen.description.clone()),
                                )
                            })
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(cx.theme().muted_foreground)
                                    .truncate()
                                    .child(progress_of(helper)),
                            ),
                    )
                    .child(self.helper_controls(seen, true, cx)),
            )
            .when(self.steering == Some(seen.id), |this| {
                this.child(self.steer_box(cx))
            })
    }

    /// Open, while `open` is offered, then Steer and Stop while it works.
    fn helper_controls(&self, seen: &Seen, open: bool, cx: &mut Context<Self>) -> impl IntoElement {
        let id = seen.id;
        let working = seen.ended.is_none();
        h_flex()
            .flex_shrink_0()
            .gap_1()
            .items_center()
            .when(open, |this| {
                this.child(
                    Button::new(("helper-open", id as usize))
                        .ghost()
                        .xsmall()
                        .icon(Icon::new(AppIcon::View))
                        .label("Open")
                        .tooltip("Read what it has done so far")
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.open_transcript(id, window, cx)
                        })),
                )
            })
            .when(working, |this| {
                this.child(
                    Button::new(("helper-steer", id as usize))
                        .ghost()
                        .xsmall()
                        .icon(Icon::new(AppIcon::MessageSquare))
                        .label("Steer")
                        .tooltip("Send it a message it reads at its next step")
                        .on_click(
                            cx.listener(move |this, _, window, cx| this.open_steer(id, window, cx)),
                        ),
                )
                .child(
                    Button::new(("helper-stop", id as usize))
                        .ghost()
                        .xsmall()
                        .icon(Icon::new(AppIcon::Stop))
                        .label("Stop")
                        .tooltip("Stop it for good: nothing picks it back up")
                        .on_click(
                            cx.listener(move |this, _, window, cx| {
                                this.stop_helper(id, window, cx)
                            }),
                        ),
                )
            })
    }

    fn steer_box(&self, cx: &mut Context<Self>) -> impl IntoElement {
        h_flex()
            .w_full()
            .gap_2()
            .items_center()
            .child(Input::new(&self.steer).small().flex_1())
            .child(
                Button::new("steer-send")
                    .primary()
                    .small()
                    .label("Send")
                    .on_click(cx.listener(|this, _, window, cx| this.send_steer(window, cx))),
            )
            .child(
                Button::new("steer-cancel")
                    .ghost()
                    .small()
                    .label("Cancel")
                    .on_click(cx.listener(|this, _, window, cx| this.close_steer(window, cx))),
            )
    }

    /// A helper's transcript as it stands, read-only, under its name and
    /// the same controls as its row.
    fn transcript_view(
        &self,
        viewing: &Viewing,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let helper = self
            .helpers
            .iter()
            .find(|helper| helper.seen.id == viewing.id);
        let rows: Vec<AnyElement> = (viewing.messages.iter().enumerate())
            .map(|(index, message)| self.transcript_row(viewing.id, index, message, window, cx))
            .collect();
        v_flex()
            .track_focus(&self.activity_focus)
            .flex_1()
            .min_h_0()
            .px_8()
            .py_6()
            .gap_4()
            .child(
                h_flex()
                    .w_full()
                    .items_center()
                    .gap_3()
                    .child(
                        Button::new("helper-back")
                            .ghost()
                            .small()
                            .icon(Icon::new(IconName::ArrowLeft))
                            .tooltip("Back to every helper")
                            .on_click(
                                cx.listener(|this, _, window, cx| this.open_activity(window, cx)),
                            ),
                    )
                    .children(helper.map(|helper| {
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .gap_0p5()
                            .child(
                                div()
                                    .text_lg()
                                    .font_family(HEADING_FONT)
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child(format!("{} ({})", helper.seen.name, helper.seen.kind)),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(cx.theme().muted_foreground)
                                    .truncate()
                                    .child(progress_of(helper)),
                            )
                    }))
                    .children(helper.map(|helper| self.helper_controls(&helper.seen, false, cx))),
            )
            .when(self.steering == Some(viewing.id), |this| {
                this.child(self.steer_box(cx))
            })
            .child(
                div()
                    .relative()
                    .flex_1()
                    .min_h_0()
                    .child(
                        div()
                            .id("helper-transcript")
                            .size_full()
                            .overflow_y_scroll()
                            .track_scroll(&viewing.scroll)
                            .child(v_flex().pr_4().gap_4().children(rows)),
                    )
                    .vertical_scrollbar(&viewing.scroll),
            )
    }

    /// One message of a transcript: its task, a message it was sent, a
    /// reply with the calls it asked for, or one quiet line for a call's result.
    fn transcript_row(
        &self,
        id: AgentId,
        index: usize,
        message: &Message,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let muted = cx.theme().muted_foreground;
        if message.role == "tool" {
            let (status, color) = if message.is_error {
                (tools::one_line(&message.content), cx.theme().danger)
            } else {
                (lines(message.content.lines().count()), muted)
            };
            return h_flex()
                .min_w_0()
                .w_full()
                .px_2()
                .gap_1p5()
                .items_center()
                .text_xs()
                .font_family(HEADING_FONT)
                .text_color(muted)
                .child(
                    div()
                        .text_color(cx.theme().foreground)
                        .child(message.name.clone()),
                )
                .child(
                    div()
                        .font_family(MONO_FONT)
                        .min_w_0()
                        .max_w(relative(0.6))
                        .truncate()
                        .child(tools::summary(&message.name, &message.args)),
                )
                .child(div().min_w_0().text_color(color).truncate().child(status))
                .into_any_element();
        }
        let is_user = message.role == "user";
        let label = match (is_user, index) {
            (true, 0) => "Task".to_owned(),
            (true, _) => "Message".to_owned(),
            _ if message.model.is_empty() => "Reply".to_owned(),
            _ => message.model.clone(),
        };
        // Its own ids, so its parsed markdown never shows in a session's chat.
        let md = (SharedString::from(format!("helper-md-{id}")), index);
        let calls: Vec<AnyElement> = (message.tool_calls.iter())
            .map(|call| {
                h_flex()
                    .min_w_0()
                    .gap_1p5()
                    .text_xs()
                    .text_color(muted)
                    .child(format!("→ {}", call.name))
                    .child(
                        div()
                            .font_family(MONO_FONT)
                            .min_w_0()
                            .truncate()
                            .child(tools::summary(&call.name, &call.arguments)),
                    )
                    .into_any_element()
            })
            .collect();
        v_flex()
            .w_full()
            .gap_1p5()
            .child(
                div()
                    .text_xs()
                    .font_family(HEADING_FONT)
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(if is_user { muted } else { cx.theme().primary })
                    .child(label),
            )
            .when(!message.content.is_empty(), |this| {
                this.child(
                    div().w_full().overflow_hidden().child(
                        TextView::markdown(md, message.content.clone(), window, cx)
                            .selectable(!window.has_active_dialog(cx)),
                    ),
                )
            })
            .children(calls)
            .into_any_element()
    }
}

/// Green while it works, red when it failed, grey once it has otherwise ended.
fn state_dot(seen: &Seen, cx: &App) -> impl IntoElement {
    let color = match seen.ended.as_deref() {
        None => cx.theme().green,
        Some("failed") => cx.theme().danger,
        Some(_) => cx.theme().muted_foreground.opacity(0.5),
    };
    div().size(px(8.)).rounded_full().flex_shrink_0().bg(color)
}

/// The helpers that are working, by id.
fn working(helpers: &[Helper]) -> Vec<AgentId> {
    (helpers.iter())
        .filter(|helper| helper.seen.ended.is_none())
        .map(|helper| helper.seen.id)
        .collect()
}

/// The helpers as `now` has them, each with how long it has run. One that
/// had ended by the last read keeps the time it had then, since its clock
/// would otherwise run on.
fn refreshed(before: &[Helper], now: Vec<Seen>) -> Vec<Helper> {
    now.into_iter()
        .map(|seen| {
            // The same run, since a helper picked back up starts a new one.
            let last = (before.iter())
                .find(|old| old.seen.id == seen.id && old.seen.started == seen.started);
            let ended = seen.ended.is_some();
            let seconds = match last {
                Some(old) if ended && old.seen.ended.is_some() => old.seconds,
                None if ended => None,
                _ => Some(seen.started.elapsed().as_secs()),
            };
            Helper { seen, seconds }
        })
        .collect()
}

/// A helper's line of progress, from what it last read as.
fn progress_of(helper: &Helper) -> String {
    let status = &helper.seen.status;
    progress(
        helper.seen.ended.as_deref(),
        helper.seconds,
        status.rounds.load(Ordering::Relaxed),
        status.input.load(Ordering::Relaxed),
        status.output.load(Ordering::Relaxed),
    )
}

/// Where a helper has got to and for how long, then what it has used, once
/// it has used anything: "working · 1m12s · 6 rounds · 38k in · 2k out".
fn progress(
    ended: Option<&str>,
    seconds: Option<u64>,
    rounds: usize,
    input: u64,
    output: u64,
) -> String {
    let mut parts = vec![ended.unwrap_or("working").to_owned()];
    parts.extend(seconds.map(took));
    match rounds {
        0 => {}
        1 => parts.push("1 round".into()),
        n => parts.push(format!("{n} rounds")),
    }
    if input + output > 0 {
        parts.push(format!("{} in", count(input)));
        parts.push(format!("{} out", count(output)));
    }
    parts.join(" · ")
}

/// How long a helper has run, as the agents tool lists it: 38s, 1m12s, 2h05m.
fn took(seconds: u64) -> String {
    match seconds {
        0..60 => format!("{seconds}s"),
        60..3_600 => format!("{}m{:02}s", seconds / 60, seconds % 60),
        _ => format!("{}h{:02}m", seconds / 3_600, seconds / 60 % 60),
    }
}

/// A token count as a person says it: 512, 38k, 1.2M.
fn count(tokens: u64) -> String {
    match tokens {
        0..1_000 => tokens.to_string(),
        1_000..1_000_000 => format!("{}k", tokens / 1_000),
        _ => format!("{}.{}M", tokens / 1_000_000, tokens / 100_000 % 10),
    }
}

/// How long a call's result is, as the chat's tool rows say it.
fn lines(n: usize) -> String {
    if n == 1 {
        "1 line".into()
    } else {
        format!("{n} lines")
    }
}

#[cfg(test)]
mod tests {
    // Not `use super::*`: the prelude re-exports gpui, whose `test` macro
    // would shadow the built-in attribute.
    use super::{Helper, count, progress, refreshed, took};
    use letronna_core::agents::Seen;
    use std::time::{Duration, Instant};

    fn seen(id: u64, started: Instant, ended: Option<&str>) -> Seen {
        Seen {
            id,
            session: 1,
            name: "explore".into(),
            kind: "explore".into(),
            description: String::new(),
            n: 1,
            ended: ended.map(str::to_owned),
            started,
            status: Default::default(),
        }
    }

    #[test]
    fn a_working_helper_says_how_long_and_how_much() {
        assert_eq!(
            progress(None, Some(72), 6, 38_211, 2_104),
            "working · 1m12s · 6 rounds · 38k in · 2k out"
        );
        assert_eq!(progress(None, Some(3), 0, 0, 0), "working · 3s");
    }

    #[test]
    fn an_ended_helper_says_how_it_ended() {
        assert_eq!(
            progress(Some("stopped by the user"), Some(7_500), 1, 950, 0),
            "stopped by the user · 2h05m · 1 round · 950 in · 0 out"
        );
        assert_eq!(progress(Some("failed"), None, 0, 0, 0), "failed");
    }

    #[test]
    fn times_and_counts_read_as_a_person_says_them() {
        assert_eq!(
            [took(59), took(60), took(3_599)],
            ["59s", "1m00s", "59m59s"]
        );
        assert_eq!(
            [count(999), count(38_211), count(1_250_000)],
            ["999", "38k", "1.2M"]
        );
    }

    #[test]
    fn an_ended_helper_keeps_the_time_it_took() {
        let Some(started) = Instant::now().checked_sub(Duration::from_secs(90)) else {
            return;
        };
        let before = [
            Helper {
                seen: seen(7, started, Some("finished")),
                seconds: Some(38),
            },
            Helper {
                seen: seen(8, started, None),
                seconds: Some(89),
            },
        ];
        let now = vec![
            seen(7, started, Some("finished")),
            seen(8, started, Some("finished")),
            seen(9, started, None),
            seen(10, started, Some("failed")),
        ];
        let after = refreshed(&before, now);
        let seconds: Vec<Option<u64>> = after.iter().map(|helper| helper.seconds).collect();
        assert_eq!(seconds[0], Some(38));
        // Ended since the last read, a tick ago: its clock stops now.
        assert!(seconds[1].is_some_and(|s| s >= 90), "{seconds:?}");
        assert!(seconds[2].is_some_and(|s| s >= 90), "{seconds:?}");
        // First read already ended, its time is not known.
        assert_eq!(seconds[3], None);
        // Picked back up, it runs a new clock.
        let resumed = refreshed(&after, vec![seen(7, Instant::now(), None)]);
        assert!(resumed[0].seconds.is_some_and(|s| s < 38));
    }
}
