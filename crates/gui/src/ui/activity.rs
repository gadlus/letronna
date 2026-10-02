//! The Activity page: the helpers this window's agents started, how each is
//! getting on, and a way to read, steer or stop one.
//!
//! Core keeps its helpers in a registry and their counts in atomics, so
//! nothing tells the window when one moves on. The window reads them four
//! times a second while a turn runs, since only a turn starts one, or while
//! one works, and not at all otherwise, so an idle window runs no timer. The
//! page, the strip over the composer, the status bar's count and the
//! sidebar's dots all draw from that last read. Core knows when a run started
//! but not when it ended, so a helper's clock stops at the first read that
//! finds it ended, and one never seen working shows no time at all.
//!
//! The strip is the one place, where the next message is written, that says
//! what still works: helpers, workflow runs, background jobs, runs beside the
//! chat, and the session's plan. At rest it is one line counting them, with
//! the step under way or the newest work after the counts; Enter or a click
//! opens the list of them, each row going where that work lives. Nothing in
//! it ticks: jobs, runs and the plan redraw the window as they change, and
//! its clocks move only while its list is open, once a second, so a closed
//! strip costs a redraw only as something starts or ends.

use super::PageColumn;
use crate::prelude::*;
use crate::ui::workflows::{Ask, asked, standing};
use gpui_component::tooltip::Tooltip;
use letronna_core::agents::{self, Seen};
use letronna_core::driver::AgentId;
use letronna_core::jobs::{self, JobInfo};
use letronna_core::messaging::Source;
use letronna_core::tools;
use letronna_core::workflow::{self, RunState, RunView};
use std::sync::atomic::Ordering;
use std::time::Duration;

actions!(activity, [ShowRunning, LeaveStrip]);

/// Four reads a second: a count a person can watch move, and a helper that
/// ends shows it within a quarter of a second.
const TICK: Duration = Duration::from_millis(250);

/// A line of the strip and a row of its list: small text with room round
/// it, as a folded run of calls is drawn.
const STRIP_ROW: Pixels = px(26.);

/// Rows the strip's open list shows before it scrolls: a busy session's
/// worth, while the chat keeps most of the window.
const STRIP_ROWS: f32 = 8.;

/// The strip's open list redraws every fourth tick: its clocks count whole
/// seconds, and the rest of the window need not be drawn four times for one.
const CLOCK_TICKS: u32 = 4;

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

/// Everything the strip over the composer lists, gathered once a draw.
struct Running<'a> {
    /// Helpers working for the session on screen.
    here: Vec<&'a Helper>,
    /// Helpers working for other sessions.
    elsewhere: Vec<&'a Helper>,
    /// The session's workflow runs still running.
    workflows: Vec<RunView>,
    /// Background jobs still running, the agent's and the user's.
    jobs: Vec<JobInfo>,
    /// Runs beside the chat, by session: `/bg`, chat apps and schedules.
    runs: Vec<(u64, &'a crate::runs::Run)>,
    /// The session's plan.
    todos: &'a [Todo],
}

impl Running<'_> {
    /// How many of each thing it holds.
    fn counts(&self) -> Counts {
        Counts {
            helpers: self.here.len(),
            elsewhere: self.elsewhere.len(),
            workflows: self.workflows.len(),
            jobs: self.jobs.len(),
            runs: self.runs.len(),
            done: (self.todos.iter())
                .filter(|todo| todo.status == "done")
                .count(),
            steps: self.todos.len(),
        }
    }
}

/// How many of each thing the strip counts.
#[derive(Clone, Copy, Default)]
struct Counts {
    helpers: usize,
    elsewhere: usize,
    workflows: usize,
    jobs: usize,
    runs: usize,
    /// Steps of the plan done, and in all.
    done: usize,
    steps: usize,
}

impl Counts {
    /// How many things are working; the plan is not one of them.
    fn live(&self) -> usize {
        self.helpers + self.elsewhere + self.workflows + self.jobs + self.runs
    }

    /// Whether the strip shows at all.
    fn any(&self) -> bool {
        self.live() > 0 || self.steps > 0
    }

    /// Whether the list opens by itself: the plan has steps to go and
    /// nothing else works, so the plan is all the strip has to show.
    fn opens_by_itself(&self) -> bool {
        self.done < self.steps && self.live() == 0
    }
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
            let mut ticks = 0u32;
            loop {
                cx.background_executor().timer(TICK).await;
                ticks = ticks.wrapping_add(1);
                let going = this.update(cx, |this, cx| {
                    let moved = this.read_helpers();
                    let reloaded = this.reload_transcript();
                    // Away from the page only the strip, the status bar's
                    // count and the dots show helpers, and they change only
                    // as one starts or ends, but for the clocks of the
                    // strip's open list.
                    let ticking = this.page == Page::Activity && this.helpers_working() > 0;
                    let clocks = ticks.is_multiple_of(CLOCK_TICKS) && this.strip_clocks();
                    if moved || reloaded || ticking || clocks {
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
    /// that starts one, or one is working. Or the strip's open list has a
    /// workflow run's clock to move.
    fn helpers_may_move(&self) -> bool {
        self.busy || self.helpers_working() > 0 || self.strip_clocks()
    }

    /// Whether the strip's open list shows a clock that moves: a working
    /// helper's, or a running workflow's.
    fn strip_clocks(&self) -> bool {
        self.page == Page::Chat
            && self.running_open
            && (self.helpers_working() > 0
                || (workflow::runs(self.session_on_screen()).iter())
                    .any(|run| run.state == RunState::Running))
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
        // Its Steer and Stop go once it has ended. Stopped from the strip,
        // the keys go back where the next message is written.
        if self.page == Page::Chat {
            self.focus_composer(window, cx);
        } else {
            self.focus_activity(window);
        }
        cx.notify();
    }

    /// Helper `id`'s name, as the last read had it.
    fn helper_name(&self, id: AgentId) -> String {
        (self.helpers.iter())
            .find(|helper| helper.seen.id == id)
            .map_or("The helper".to_owned(), |helper| helper.seen.name.clone())
    }

    /// What the strip lists now.
    fn running(&self) -> Running<'_> {
        let session = self.session_on_screen();
        let (here, elsewhere) = (self.helpers.iter())
            .filter(|helper| helper.seen.ended.is_none())
            .partition(|helper| helper.seen.session == session);
        Running {
            here,
            elsewhere,
            workflows: (workflow::runs(session).into_iter())
                .filter(|run| run.state == RunState::Running)
                .collect(),
            jobs: (jobs::list().into_iter())
                .filter(|job| job.running)
                .collect(),
            runs: (self.runs.iter()).map(|(&id, run)| (id, run)).collect(),
            todos: &self.store.conversations[self.store.active].todos,
        }
    }

    /// How many things are working, for the status bar away from the chat.
    pub(crate) fn running_count(&self) -> usize {
        self.running().counts().live()
    }

    /// Whether the strip is drawn over the composer.
    pub(crate) fn strip_shown(&self) -> bool {
        self.page == Page::Chat && self.running().counts().any()
    }

    /// Whether the strip's list is open: opened by hand, or by itself for a
    /// plan with steps to go, unless it was closed by hand in this session.
    fn strip_open(&self, counts: &Counts) -> bool {
        self.running_open
            || (counts.opens_by_itself() && !self.plan_folded.contains(&self.session_on_screen()))
    }

    /// Opens the strip's list, or closes it.
    fn toggle_strip(&mut self, cx: &mut Context<Self>) {
        let counts = self.running().counts();
        let session = self.session_on_screen();
        if self.strip_open(&counts) {
            self.running_open = false;
            if counts.opens_by_itself() {
                self.plan_folded.insert(session);
            }
        } else {
            self.running_open = true;
            self.plan_folded.remove(&session);
        }
        cx.notify();
    }

    /// The chat, with the strip's list open and the keys on its first row.
    pub(crate) fn show_running(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if window.has_active_dialog(cx) || self.settings_open {
            return;
        }
        if !self.running().counts().any() {
            self.note = Some("Nothing is running now.".into());
            cx.notify();
            return;
        }
        self.page = Page::Chat;
        self.running_open = true;
        self.plan_folded.remove(&self.session_on_screen());
        self.strip_focus_first = true;
        cx.notify();
    }

    /// Escape in the strip closes its list and gives the keys back to the
    /// composer, rather than stopping the reply as it would there.
    pub(crate) fn leave_strip(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.strip_open(&self.running().counts()) {
            self.toggle_strip(cx);
        }
        self.focus_composer(window, cx);
    }

    /// The line over the composer while anything works or the session has a
    /// plan, counting them; Enter or a click opens the list of them above it.
    pub(crate) fn running_strip(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let focus_first = std::mem::take(&mut self.strip_focus_first);
        let running = self.running();
        let counts = running.counts();
        if !counts.any() {
            return None;
        }
        let open = self.strip_open(&counts);
        // Above the line, so the line stays under the pointer that opened
        // the list and closes it again.
        let list = open.then(|| {
            self.strip_list(&running, focus_first, window, cx)
                .into_any_element()
        });
        let theme = cx.theme();
        let (muted, secondary) = (theme.muted_foreground, theme.secondary);
        let session = self.session_on_screen();
        let replying = self.busy && self.streaming_id == Some(session);
        // Steady rather than turning: a job can run for hours, and a mark
        // that moves would draw the window again all that while.
        let mark = if counts.live() > 0 || (replying && counts.done < counts.steps) {
            div()
                .size(px(6.))
                .rounded_full()
                .bg(theme.green)
                .into_any_element()
        } else if counts.done == counts.steps {
            Icon::new(IconName::CircleCheck)
                .size_3()
                .text_color(theme.green)
                .into_any_element()
        } else {
            Icon::new(AppIcon::Checklist)
                .size_3()
                .text_color(muted)
                .into_any_element()
        };
        let line = h_flex()
            .id("running-line")
            .track_focus(&self.strip_focus)
            .h(STRIP_ROW)
            .w_full()
            .min_w_0()
            // In by the list's border, so its mark lines up with the rows'.
            .pl(px(9.))
            .pr_2()
            .gap_2()
            .items_center()
            .rounded(theme.radius)
            .cursor_pointer()
            .text_xs()
            .focus(|s| s.bg(secondary))
            .active(|s| s.bg(secondary.opacity(0.5)))
            .tooltip(move |window, cx| {
                if open {
                    Tooltip::new("Hide the list")
                        .action(&LeaveStrip, Some("LiveStrip"))
                        .build(window, cx)
                } else {
                    Tooltip::new("See each one, to go to it or stop it")
                        .action(&ShowRunning, Some("Chat"))
                        .build(window, cx)
                }
            })
            .child(slot(mark))
            .child(
                div()
                    .min_w_0()
                    .truncate()
                    .text_color(theme.foreground)
                    .child(summary(&counts)),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_color(muted)
                    .children(newest_line(running.todos, self.strip_lines(&running))),
            )
            .child(
                Icon::new(if open {
                    IconName::ChevronDown
                } else {
                    IconName::ChevronRight
                })
                .size_3()
                .text_color(muted),
            )
            // As a folded run's line: a press leaves the keys where they
            // were, in the composer most often; Tab is what picks it out.
            .on_any_mouse_down(|_, window, _| window.prevent_default())
            // Enter and Space click it too, once it has the keys.
            .on_click(cx.listener(|this, _, _, cx| this.toggle_strip(cx)));
        Some(
            v_flex()
                .track_focus(&self.strip_area)
                .key_context("LiveStrip")
                .on_action(
                    cx.listener(|this, _: &LeaveStrip, window, cx| this.leave_strip(window, cx)),
                )
                .on_any_mouse_down(|_, window, _| window.prevent_default())
                .relative()
                .w_full()
                .pb_1()
                .children(list)
                .child(line)
                .children(self.ring(crate::ui::keys::Region::Strip, cx))
                .into_any_element(),
        )
    }

    /// What each thing working last did or was asked, with how long ago it
    /// started, for the strip's line to show the newest.
    fn strip_lines(&self, running: &Running) -> Vec<(Duration, String)> {
        let helpers = (running.here.iter().chain(&running.elsewhere)).map(|helper| {
            let seen = &helper.seen;
            let line = if seen.description.is_empty() {
                seen.name.clone()
            } else {
                format!("{}: {}", seen.name, seen.description)
            };
            (seen.started.elapsed(), line)
        });
        let workflows = (running.workflows.iter())
            .map(|run| (Duration::from_secs(run.seconds), run.title.clone()));
        let jobs = (running.jobs.iter()).map(|job| (job.took, tools::one_line(&job.command)));
        // A run says nothing of when it began: after the rest.
        let runs = (running.runs.iter()).map(|(_, run)| (Duration::MAX, run.label.clone()));
        helpers.chain(workflows).chain(jobs).chain(runs).collect()
    }

    /// The strip's open list: its helpers, workflow runs, jobs, runs beside
    /// the chat and the plan, in the order the line counts them.
    fn strip_list(
        &self,
        running: &Running,
        focus_first: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let mut rows: Vec<AnyElement> = Vec::new();
        let mut first: Option<FocusHandle> = None;
        for helper in running.here.iter().chain(&running.elsewhere) {
            let focus = row_focus(("strip-helper-focus", helper.seen.id as usize), window, cx);
            rows.push(self.strip_helper(helper, &focus, cx).into_any_element());
            first.get_or_insert(focus);
        }
        for (index, run) in running.workflows.iter().enumerate() {
            let focus = row_focus(("strip-workflow-focus", index), window, cx);
            rows.push(self.strip_run(index, run, &focus, cx).into_any_element());
            first.get_or_insert(focus);
        }
        for job in &running.jobs {
            let focus = row_focus(("strip-job-focus", job.id as usize), window, cx);
            rows.push(self.strip_job(job, &focus, cx).into_any_element());
            first.get_or_insert(focus);
        }
        for &(id, run) in &running.runs {
            // A scheduled run's session comes in only once it ends, so its
            // row goes nowhere.
            let focus =
                (run.held).then(|| row_focus(("strip-beside-focus", id as usize), window, cx));
            rows.push(self.strip_beside(id, run, focus.as_ref(), cx));
            if let Some(focus) = focus {
                first.get_or_insert(focus);
            }
        }
        rows.extend(
            (running.todos.iter().enumerate())
                .map(|(i, todo)| todo_row(i, todo, cx).into_any_element()),
        );
        if focus_first {
            window.focus(first.as_ref().unwrap_or(&self.strip_focus));
        }
        v_flex()
            .id("running-list")
            .w_full()
            .max_h(STRIP_ROW * STRIP_ROWS + px(10.))
            .mb_1()
            .py_1()
            .rounded(cx.theme().radius)
            .border_1()
            .border_color(cx.theme().border)
            .bg(cx.theme().secondary.opacity(0.3))
            .overflow_y_scrollbar()
            .children(rows)
    }

    /// A working helper as the strip lists it, with the session it works for
    /// when that is not the one on screen; it opens on the Activity page.
    fn strip_helper(
        &self,
        helper: &Helper,
        focus: &FocusHandle,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let seen = &helper.seen;
        let id = seen.id;
        let mut about = Vec::new();
        if seen.session != self.session_on_screen() {
            let title = (self.store.index_of(seen.session))
                .map_or("another session", |ix| &self.store.conversations[ix].title);
            about.push(format!("in \u{201c}{title}\u{201d}"));
        }
        if !seen.description.is_empty() {
            about.push(seen.description.clone());
        }
        let row = strip_row(
            seen.name.clone(),
            about.join(" · "),
            progress_of(helper),
            cx,
        );
        goes(
            row,
            ("strip-helper", id as usize),
            focus,
            cx,
            move |this, window, cx| {
                this.page = Page::Activity;
                this.open_transcript(id, window, cx);
            },
        )
        .child(
            Button::new(("strip-stop", id as usize))
                .plain(cx)
                .xsmall()
                .label("Stop")
                .tooltip("Stop it for good: nothing picks it back up")
                .on_click(cx.listener(move |this, _, window, cx| {
                    // Stopping it is not opening it: the row's own click
                    // must not follow.
                    cx.stop_propagation();
                    this.stop_helper(id, window, cx)
                })),
        )
    }

    /// A running workflow of the session on screen as the strip lists it; it
    /// opens on the Workflows page.
    fn strip_run(
        &self,
        index: usize,
        run: &RunView,
        focus: &FocusHandle,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let session = self.session_on_screen();
        let (open, stop) = (run.name.clone(), run.name.clone());
        let about = format!("workflow {}", run.name);
        let row = strip_row(run.title.clone(), about, standing(run), cx);
        goes(
            row,
            ("strip-workflow", index),
            focus,
            cx,
            move |this, window, cx| {
                this.page = Page::Workflows;
                this.show_run(Some(open.clone()), window, cx);
            },
        )
        .child(
            Button::new(("strip-run-stop", index))
                .plain(cx)
                .xsmall()
                .label("Stop")
                .tooltip("Stop the run and its agents")
                .on_click(cx.listener(move |this, _, window, cx| {
                    cx.stop_propagation();
                    if let Err(error) = asked(session, &stop, Ask::Stop) {
                        this.error = Some(error);
                    }
                    this.focus_composer(window, cx);
                    cx.notify();
                })),
        )
    }

    /// A background job as the strip lists it; it opens in the Jobs tab.
    fn strip_job(
        &self,
        job: &JobInfo,
        focus: &FocusHandle,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let id = job.id;
        let whose = if job.yours { "yours" } else { "the agent's" };
        let row = strip_row(
            tools::one_line(&job.command),
            format!("job #{id} · {whose}"),
            job.state(),
            cx,
        );
        goes(
            row,
            ("strip-job", id as usize),
            focus,
            cx,
            move |this, window, cx| this.show_job(id, window, cx),
        )
        .child(
            Button::new(("strip-job-stop", id as usize))
                .plain(cx)
                .xsmall()
                .label("Stop")
                .tooltip("Stop the command")
                .on_click(cx.listener(move |this, _, window, cx| {
                    cx.stop_propagation();
                    if let Err(error) = jobs::stop(id) {
                        this.error = Some(error);
                    }
                    this.focus_composer(window, cx);
                    cx.notify();
                })),
        )
    }

    /// A run beside the chat as the strip lists it; a held one opens its
    /// session, while a scheduled one's comes in only once it ends.
    fn strip_beside(
        &self,
        id: u64,
        run: &crate::runs::Run,
        focus: Option<&FocusHandle>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let stopping = run.cancel.is_set();
        let state = match (run.queued, stopping) {
            (true, _) => "waiting",
            (_, true) => "stopping",
            _ => "running",
        };
        let about = match (&run.source, run.held) {
            (Some(Source::Telegram(_)), _) => "from Telegram",
            (Some(Source::Discord(_)), _) => "from Discord",
            (Some(Source::Slack { .. }), _) => "from Slack",
            (None, true) => "beside the chat",
            (None, false) => "scheduled",
        };
        let row = strip_row(run.label.clone(), about.into(), state.into(), cx);
        let stop = Button::new(("strip-beside-stop", id as usize))
            .plain(cx)
            .xsmall()
            .label("Stop")
            .tooltip("Stop it; what it did so far is kept")
            .disabled(stopping)
            .on_click(cx.listener(move |this, _, window, cx| {
                cx.stop_propagation();
                this.stop_run(id, cx);
                this.focus_composer(window, cx);
            }));
        let Some(focus) = focus else {
            return row.child(stop).into_any_element();
        };
        goes(
            row,
            ("strip-beside", id as usize),
            focus,
            cx,
            move |this, window, cx| {
                let Some(ix) = this.store.index_of(id) else {
                    return;
                };
                this.leave_for(ix);
                this.store.active = ix;
                this.error = None;
                this.page = Page::Chat;
                this.follow();
                this.focus_composer(window, cx);
                this.save();
                cx.notify();
            },
        )
        .child(stop)
        .into_any_element()
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
            .page_column()
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
                        .plain(cx)
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
                        .plain(cx)
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
                        .plain(cx)
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
                    .plain(cx)
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
            .page_column()
            .py_6()
            .gap_4()
            .child(
                h_flex()
                    .w_full()
                    .items_center()
                    .gap_3()
                    .child(
                        Button::new("helper-back")
                            .plain(cx)
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

/// A row of the strip's list: a green dot, a name, what it is about and how
/// it is getting on, then the buttons the caller adds.
fn strip_row(name: String, about: String, progress: String, cx: &App) -> Div {
    let muted = cx.theme().muted_foreground;
    h_flex()
        .w_full()
        .h(STRIP_ROW)
        .pl_2()
        .pr_1()
        .gap_2()
        .items_center()
        .text_xs()
        .child(slot(div().size(px(6.)).rounded_full().bg(cx.theme().green)))
        .child(
            div()
                .flex_shrink_0()
                .max_w(px(200.))
                .truncate()
                .font_weight(FontWeight::MEDIUM)
                .child(name),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .truncate()
                .text_color(muted)
                .child(about),
        )
        .child(div().flex_shrink_0().text_color(muted).child(progress))
}

/// `mark` centred in the width of a small icon, so the text after a dot
/// starts where the text after a step's check does.
fn slot(mark: impl IntoElement) -> Div {
    h_flex().w_3().flex_shrink_0().justify_center().child(mark)
}

/// `row` made a tab stop that goes where its work lives on a click, or on
/// Enter or Space: gpui clicks a focused element as either comes back up,
/// so the key does not also press whatever the keys go to.
fn goes(
    row: Div,
    id: impl Into<ElementId>,
    focus: &FocusHandle,
    cx: &mut Context<Chat>,
    go: impl Fn(&mut Chat, &mut Window, &mut Context<Chat>) + 'static,
) -> Stateful<Div> {
    let secondary = cx.theme().secondary;
    row.id(id)
        .track_focus(focus)
        .rounded(cx.theme().radius)
        .cursor_pointer()
        .focus(|s| s.bg(secondary))
        .active(|s| s.bg(secondary.opacity(0.5)))
        // As a folded run's line: a press leaves the keys where they were.
        .on_any_mouse_down(|_, window, _| window.prevent_default())
        .on_click(cx.listener(move |this, _, window, cx| go(this, window, cx)))
}

/// The focus of the strip's row `key`, a tab stop, kept while it is drawn.
fn row_focus(key: impl Into<ElementId>, window: &mut Window, cx: &mut App) -> FocusHandle {
    (window.use_keyed_state(key, cx, |_, cx| cx.focus_handle().tab_stop(true)))
        .read(cx)
        .clone()
}

/// A step of the plan as the strip lists it: done, under way or to do.
fn todo_row(index: usize, todo: &Todo, cx: &App) -> impl IntoElement {
    let theme = cx.theme();
    // The three states use icons from different sets, so build the
    // element in each arm rather than passing a name around.
    let (icon, color) = match todo.status.as_str() {
        "done" => (Icon::new(IconName::CircleCheck), theme.green),
        "doing" => (Icon::new(IconName::LoaderCircle), theme.link),
        _ => (Icon::new(AppIcon::Circle), theme.muted_foreground),
    };
    let text = todo.text.clone();
    h_flex()
        .id(("strip-todo", index))
        .w_full()
        .h(STRIP_ROW)
        .pl_2()
        .pr_1()
        .gap_2()
        .items_center()
        .text_xs()
        .child(slot(icon.size_3().text_color(color)))
        .child(
            div()
                .min_w_0()
                .truncate()
                .when(todo.status == "done", |d| {
                    d.line_through().text_color(theme.muted_foreground)
                })
                .child(todo.text.clone()),
        )
        // Cut to one line, so the whole step shows under the pointer.
        .tooltip(move |window, cx| Tooltip::new(text.clone()).build(window, cx))
}

/// What the strip's line counts, in a fixed order, as in
/// "2 helpers · 1 job · 1 run · Plan 2/5".
fn summary(counts: &Counts) -> String {
    let some = |n: usize, one: &str, many: &str| match n {
        0 => None,
        1 => Some(format!("1 {one}")),
        n => Some(format!("{n} {many}")),
    };
    // Alone, they say what they are; after the session's own, only how many.
    let elsewhere = if counts.helpers == 0 {
        some(
            counts.elsewhere,
            "helper in another session",
            "helpers in other sessions",
        )
    } else {
        some(counts.elsewhere, "elsewhere", "elsewhere")
    };
    let plan = (counts.steps > 0).then(|| format!("Plan {}/{}", counts.done, counts.steps));
    [
        some(counts.helpers, "helper", "helpers"),
        elsewhere,
        some(counts.workflows, "workflow", "workflows"),
        some(counts.jobs, "job", "jobs"),
        some(counts.runs, "run", "runs"),
        plan,
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>()
    .join(" · ")
}

/// What the strip's line says after its counts: the step of the plan under
/// way, or else the newest of `lines`, each with how long ago it started.
fn newest_line(todos: &[Todo], lines: Vec<(Duration, String)>) -> Option<String> {
    if let Some(todo) = todos.iter().find(|todo| todo.status == "doing") {
        return Some(todo.text.clone());
    }
    (lines.into_iter())
        .min_by_key(|(ago, _)| *ago)
        .map(|(_, line)| line)
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
    use super::{Counts, Helper, count, newest_line, progress, refreshed, summary, took};
    use letronna_core::agents::Seen;
    use letronna_core::store::Todo;
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

    fn todo(text: &str, status: &str) -> Todo {
        Todo {
            text: text.into(),
            status: status.into(),
        }
    }

    #[test]
    fn the_strip_counts_each_kind_in_a_fixed_order() {
        let counts = Counts {
            helpers: 2,
            workflows: 1,
            jobs: 1,
            runs: 3,
            done: 2,
            steps: 5,
            ..Counts::default()
        };
        assert_eq!(
            summary(&counts),
            "2 helpers · 1 workflow · 1 job · 3 runs · Plan 2/5"
        );
        let jobs = Counts {
            jobs: 2,
            ..Counts::default()
        };
        assert_eq!(summary(&jobs), "2 jobs");
    }

    #[test]
    fn helpers_of_other_sessions_say_so_alone_and_count_after_the_sessions_own() {
        let alone = Counts {
            elsewhere: 1,
            ..Counts::default()
        };
        assert_eq!(summary(&alone), "1 helper in another session");
        let after = Counts {
            helpers: 1,
            elsewhere: 3,
            ..Counts::default()
        };
        assert_eq!(summary(&after), "1 helper · 3 elsewhere");
    }

    #[test]
    fn a_plan_with_steps_to_go_opens_the_list_only_when_nothing_else_works() {
        let plan = Counts {
            done: 2,
            steps: 5,
            ..Counts::default()
        };
        assert!(plan.opens_by_itself());
        let busy = Counts { jobs: 1, ..plan };
        assert!(!busy.opens_by_itself());
        let done = Counts { done: 5, ..plan };
        assert!(!done.opens_by_itself());
        assert!(done.any(), "a finished plan still rests as one line");
        assert!(!Counts::default().any());
    }

    #[test]
    fn the_line_shows_the_step_under_way_else_the_newest_work() {
        let lines = || {
            vec![
                (
                    Duration::from_secs(90),
                    "explore: read the panel".to_owned(),
                ),
                (Duration::from_secs(4), "cargo test".to_owned()),
                (Duration::MAX, "tidy the sidebar".to_owned()),
            ]
        };
        let plan = [todo("Read it", "done"), todo("Run the tests", "doing")];
        assert_eq!(
            newest_line(&plan, lines()).as_deref(),
            Some("Run the tests")
        );
        let waiting = [todo("Read it", "done"), todo("Run the tests", "pending")];
        assert_eq!(
            newest_line(&waiting, lines()).as_deref(),
            Some("cargo test")
        );
        assert_eq!(newest_line(&[], Vec::new()), None);
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
