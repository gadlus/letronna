//! The Jobs tab of the right panel: every command left running in the
//! background, the agent's and yours, with where it stands, and the chosen
//! one's output as it comes.
//!
//! There is no terminal emulator here. The output is the job's log as core
//! keeps it, a line each in the code font; lines are picked with the arrows
//! or the pointer to go into the message, and interactive work goes to the
//! user's own terminal, opened in the job's folder. Core rings a doorbell as
//! a job starts, prints or ends, so nothing here reads the list on a timer,
//! and the chosen job's log is read again only while the tab is on screen.

use crate::prelude::*;
use crate::ui::panel::PanelTab;
use gpui_component::dialog::DialogButtonProps;
use letronna_core::approvals::Decision;
use letronna_core::jobs::{self, JobInfo};
use std::ops::RangeInclusive;
use std::path::Path;
use std::time::Duration;

actions!(
    jobs,
    [
        JobsTab,
        NextJob,
        PreviousJob,
        LineDown,
        LineUp,
        PickDown,
        PickUp,
        PickAll,
        Unpick,
        AddLines
    ]
);

/// How tall the job list grows before it scrolls, so the output keeps most
/// of the panel.
const LIST_MAX: Pixels = px(168.);
/// How tall the tab is in the floating panel, at most, as the Files tab is.
const FLOAT_HEIGHT: Pixels = px(440.);
/// Lines of output drawn, the newest: a log of 64 KB can hold tens of
/// thousands of empty lines, and drawing them all ten times a second would
/// stall the window.
const MAX_LINES: usize = 2_000;
/// How long a burst of output gathers before the next redraw: a chatty job
/// rings on every read, and a redraw per read would keep the window busy.
const SETTLE: Duration = Duration::from_millis(100);

/// Lines picked in a job's output, from `anchor` to `cursor` either way round.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Picked {
    anchor: usize,
    cursor: usize,
}

impl Picked {
    fn one(line: usize) -> Self {
        Self {
            anchor: line,
            cursor: line,
        }
    }

    fn range(self) -> RangeInclusive<usize> {
        self.anchor.min(self.cursor)..=self.anchor.max(self.cursor)
    }

    /// The cursor moved `by` lines within `len`, taking the anchor along
    /// unless the pick is being stretched.
    fn moved(self, by: isize, stretch: bool, len: usize) -> Self {
        let cursor = self
            .cursor
            .saturating_add_signed(by)
            .min(len.saturating_sub(1));
        Self {
            anchor: if stretch { self.anchor } else { cursor },
            cursor,
        }
    }
}

/// The picked lines as one text, or None when nothing is picked.
fn picked_text(lines: &[SharedString], picked: Option<Picked>) -> Option<String> {
    let range = picked?.range();
    let chosen = lines.get(range)?;
    Some(
        chosen
            .iter()
            .map(|line| line.as_ref())
            .collect::<Vec<_>>()
            .join("\n"),
    )
}

/// The last `MAX_LINES` lines of a log.
fn tail_lines(log: &str) -> Vec<SharedString> {
    let lines: Vec<&str> = log.trim_end().lines().collect();
    lines[lines.len().saturating_sub(MAX_LINES)..]
        .iter()
        .map(|line| SharedString::from(line.to_string()))
        .collect()
}

/// `text` as a code block, fenced by more backticks than any run in it so
/// that output holding a fence of its own cannot end the block early.
fn fenced(text: &str) -> String {
    let mut longest = 0;
    let mut run = 0;
    for c in text.chars() {
        run = if c == '`' { run + 1 } else { 0 };
        longest = longest.max(run);
    }
    let fence = "`".repeat(longest.max(2) + 1);
    format!("{fence}\n{text}\n{fence}")
}

/// What the Jobs tab keeps between draws.
pub(crate) struct Jobs {
    /// The job whose output shows.
    selected: Option<u64>,
    /// Its output as last read, a line each.
    lines: Vec<SharedString>,
    /// Lines picked in it. While any are, the output holds still, so what
    /// was picked is what goes into the message.
    picked: Option<Picked>,
    /// A job has moved on, or another was chosen, since the output was read.
    stale: bool,
    /// Each job and whether it ran when the doorbell last rang, so a ring
    /// that changes neither redraws only the tab.
    listed: Vec<(u64, bool)>,
    /// The Run line, which starts a job of your own.
    run_line: Entity<InputState>,
    /// The list's, which the Jobs tab's key and a job's row in the strip
    /// over the composer hand the keys to.
    list_focus: FocusHandle,
    log_focus: FocusHandle,
    scroll: ScrollHandle,
}

impl Jobs {
    pub(crate) fn new(window: &mut Window, cx: &mut App) -> Self {
        Self {
            selected: None,
            lines: Vec::new(),
            picked: None,
            stale: true,
            listed: Vec::new(),
            run_line: cx.new(|cx| {
                InputState::new(window, cx).placeholder("Run a command in the background")
            }),
            list_focus: cx.focus_handle().tab_stop(true),
            log_focus: cx.focus_handle().tab_stop(true),
            scroll: ScrollHandle::new(),
        }
    }
}

impl Chat {
    /// Hear core's doorbell, a burst of output at a time.
    pub(crate) fn watch_jobs(window: &mut Window, cx: &mut Context<Self>) {
        let doorbell = jobs::listen();
        cx.spawn_in(window, async move |this, cx| {
            while doorbell.recv().await.is_ok() {
                let rang = this.update_in(cx, |this, window, cx| this.jobs_rang(window, cx));
                if rang.is_err() {
                    break;
                }
                // Rings meanwhile merge into one, heard after the wait.
                cx.background_executor().timer(SETTLE).await;
            }
        })
        .detach();
    }

    /// Note that the jobs moved on, and redraw what shows it: the tab while
    /// it is on screen, and the strip over the composer when a job started
    /// or ended. A job printing with the tab hidden redraws nothing.
    fn jobs_rang(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.jobs.stale = true;
        let listed: Vec<(u64, bool)> = jobs::list()
            .iter()
            .map(|job| (job.id, job.running))
            .collect();
        let on_screen = self.tab_on_screen(window, cx) == Some(PanelTab::Jobs);
        if on_screen || listed != self.jobs.listed {
            self.jobs.listed = listed;
            cx.notify();
        }
    }

    /// Give the job list the keys, as the tab is shown.
    pub(crate) fn focus_jobs(&self, window: &mut Window) {
        window.focus(&self.jobs.list_focus);
    }

    /// Read the chosen job's output again when the tab is on screen and it
    /// may have changed: a job moved on, or another was chosen.
    pub(crate) fn keep_jobs_current(&mut self, window: &Window, cx: &mut Context<Self>) {
        if !self.jobs.stale || self.tab_on_screen(window, cx) != Some(PanelTab::Jobs) {
            return;
        }
        self.jobs.stale = false;
        self.read_jobs();
    }

    /// Choose a job, following its output from the newest line.
    fn choose_job(&mut self, id: u64, cx: &mut Context<Self>) {
        self.jobs.selected = Some(id);
        self.jobs.picked = None;
        self.jobs.stale = true;
        self.jobs.scroll.scroll_to_bottom();
        cx.notify();
    }

    /// Start the Run line's command when Enter is pressed in it.
    pub(crate) fn follow_run_line(
        &self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Subscription {
        cx.subscribe_in(
            &self.jobs.run_line,
            window,
            |this, _, event: &InputEvent, window, cx| {
                if matches!(event, InputEvent::PressEnter { .. }) {
                    this.run_job(window, cx);
                }
            },
        )
    }

    /// Choose the newest job when none is chosen, and read the chosen one's
    /// output again unless lines are picked in it.
    fn read_jobs(&mut self) {
        let list = jobs::list();
        let gone = self
            .jobs
            .selected
            .is_none_or(|id| !list.iter().any(|job| job.id == id));
        if gone {
            self.jobs.selected = list.last().map(|job| job.id);
            self.jobs.picked = None;
        }
        if self.jobs.picked.is_some() {
            return;
        }
        let Some(id) = self.jobs.selected else {
            self.jobs.lines.clear();
            return;
        };
        let scroll = &self.jobs.scroll;
        // Kept at the end while it is there, and left where it was scrolled
        // back to.
        let at_end = scroll.offset().y <= -scroll.max_offset().height + px(4.);
        self.jobs.lines = tail_lines(&jobs::log(id).unwrap_or_default());
        if at_end || gone {
            scroll.scroll_to_bottom();
        }
    }

    /// Show a job's output in the Jobs tab, as its row in the strip does.
    pub(crate) fn show_job(&mut self, id: u64, window: &mut Window, cx: &mut Context<Self>) {
        self.choose_job(id, cx);
        self.show_tab(PanelTab::Jobs, window, cx);
    }

    /// Move the choice of job up or down the list, which shows the newest
    /// first.
    fn step_job(&mut self, by: isize, cx: &mut Context<Self>) {
        let list: Vec<u64> = jobs::list().iter().rev().map(|job| job.id).collect();
        let at = self
            .jobs
            .selected
            .and_then(|id| list.iter().position(|&each| each == id))
            .unwrap_or(0);
        let Some(&id) = list.get(
            at.saturating_add_signed(by)
                .min(list.len().saturating_sub(1)),
        ) else {
            return;
        };
        self.choose_job(id, cx);
    }

    /// Move or stretch the picked lines with the keys; with none picked,
    /// the newest line is picked first.
    fn pick_step(&mut self, by: isize, stretch: bool, cx: &mut Context<Self>) {
        let len = self.jobs.lines.len();
        if len == 0 {
            return;
        }
        let picked = match self.jobs.picked {
            Some(picked) => picked.moved(by, stretch, len),
            None => Picked::one(len - 1),
        };
        self.jobs.picked = Some(picked);
        self.jobs.scroll.scroll_to_item(picked.cursor);
        cx.notify();
    }

    /// Pick a line with the pointer, or stretch the pick to it with Shift.
    fn pick_line(
        &mut self,
        line: usize,
        stretch: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.jobs.picked = Some(match self.jobs.picked {
            Some(picked) if stretch => Picked {
                anchor: picked.anchor,
                cursor: line,
            },
            _ => Picked::one(line),
        });
        window.focus(&self.jobs.log_focus);
        cx.notify();
    }

    /// Let go of the picked lines, and follow the output again.
    fn unpick(&mut self, cx: &mut Context<Self>) {
        self.jobs.picked = None;
        self.jobs.stale = true;
        self.jobs.scroll.scroll_to_bottom();
        cx.notify();
    }

    /// Put the picked lines into the message being written, as a code
    /// block after what is typed, and go there.
    fn add_lines(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(text) = picked_text(&self.jobs.lines, self.jobs.picked) else {
            return;
        };
        let block = fenced(&text);
        self.composer.update(cx, |input, cx| {
            use gpui_component::input::RopeExt;
            let typed = input.value().trim_end().to_owned();
            let value = if typed.is_empty() {
                block
            } else {
                format!("{typed}\n\n{block}")
            };
            // Typing goes on under the block: setting a many-line box's
            // text leaves its cursor at the start, on the opening fence.
            let value = format!("{value}\n");
            let end = value.len();
            input.set_value(value, window, cx);
            let end = input.text().offset_to_position(end);
            input.set_cursor_position(end, window, cx);
        });
        self.composer_empty = false;
        self.unpick(cx);
        self.focus_composer(window, cx);
    }

    /// Start the Run line's command in the session's folder, as the agent's
    /// `run_command` would be: what the rules allow starts, what they refuse
    /// is refused, and the rest is asked first.
    fn run_job(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let command = self.jobs.run_line.read(cx).value().trim().to_owned();
        if command.is_empty() {
            return;
        }
        let open = &self.store.conversations[self.store.active];
        let (session, folder) = (open.id, open.workspace.clone());
        let Some(folder) = folder else {
            self.error = Some("Attach a project folder first: the command runs in it.".into());
            cx.notify();
            return;
        };
        let call = backend::ToolCall {
            id: "jobs-run-line".into(),
            name: "run_command".into(),
            arguments: serde_json::json!({"command": command, "background": true}).to_string(),
        };
        match self.decided(&call, session) {
            Some(Decision::Deny(why)) => {
                self.error = Some(if why.is_empty() {
                    "This session's rules do not let that command run.".into()
                } else {
                    why
                });
                cx.notify();
            }
            Some(_) => self.start_job(command, folder, window, cx),
            None => self.confirm_job(command, folder, window, cx),
        }
    }

    /// Ask before starting a command the session's rules do not allow by
    /// themselves.
    fn confirm_job(
        &mut self,
        command: String,
        folder: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let chat = cx.entity();
        let place = folder.display().to_string();
        window.open_dialog(cx, move |dialog, _, cx| {
            let (chat, command, folder) = (chat.clone(), command.clone(), folder.clone());
            dialog
                .title(div().font_family(HEADING_FONT).child("Run this command?"))
                .w(px(460.))
                .confirm()
                .button_props(DialogButtonProps::default().ok_text("Run"))
                .child(
                    v_flex()
                        .gap_2()
                        .child(
                            div()
                                .text_sm()
                                .font_family(MONO_FONT)
                                .child(command.clone()),
                        )
                        .child(
                            div()
                                .text_sm()
                                .text_color(cx.theme().muted_foreground)
                                .child(format!("It keeps running in the background, in {place}.")),
                        ),
                )
                .on_ok(move |_, window, cx| {
                    let (command, folder) = (command.clone(), folder.clone());
                    chat.update(cx, |this, cx| this.start_job(command, folder, window, cx));
                    true
                })
        });
    }

    fn start_job(
        &mut self,
        command: String,
        folder: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match jobs::start_yours(&command, &folder) {
            Ok(id) => {
                self.jobs
                    .run_line
                    .update(cx, |input, cx| input.set_value("", window, cx));
                self.show_job(id, window, cx);
            }
            Err(e) => self.error = Some(e),
        }
        cx.notify();
    }

    fn open_terminal_in(&mut self, folder: &Path, cx: &mut Context<Self>) {
        if let Err(e) = jobs::open_terminal(folder) {
            self.error = Some(e);
            cx.notify();
        }
    }

    /// The Jobs tab: the Run line, the list, and the chosen job's output
    /// with what can be done with it.
    pub(crate) fn jobs_tab(&self, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        let list = jobs::list();
        let chosen = self
            .jobs
            .selected
            .and_then(|id| list.iter().find(|job| job.id == id))
            .cloned();
        let docked = crate::ui::panel::docks(window.viewport_size().width);
        v_flex()
            .w_full()
            .min_h_0()
            // Floating, the panel is as tall as what it holds, and the
            // output would have no height to fill; it shrinks in a short
            // window.
            .map(|tab| match docked {
                true => tab.flex_1(),
                false => tab.h(FLOAT_HEIGHT).flex_shrink(),
            })
            .child(self.run_line(cx))
            .child(self.job_list(&list, window, cx))
            .children(chosen.map(|job| self.job_output(job, cx)))
            .on_action(cx.listener(|this, _: &NextJob, _, cx| this.step_job(1, cx)))
            .on_action(cx.listener(|this, _: &PreviousJob, _, cx| this.step_job(-1, cx)))
            .into_any_element()
    }

    fn run_line(&self, cx: &mut Context<Self>) -> impl IntoElement {
        h_flex()
            .px_3()
            .py_2()
            .gap_2()
            .border_b_1()
            .border_color(cx.theme().border)
            .child(
                Input::new(&self.jobs.run_line)
                    .small()
                    .prefix(Icon::new(AppIcon::Terminal).size_4())
                    .flex_1(),
            )
            .child(
                Button::new("run-job")
                    .framed(cx)
                    .small()
                    .label("Run")
                    .tooltip("Start it in the project folder, asking first where the rules say to")
                    .on_click(cx.listener(|this, _, window, cx| this.run_job(window, cx))),
            )
    }

    fn job_list(
        &self,
        list: &[JobInfo],
        window: &Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let focused = self.jobs.list_focus.is_focused(window);
        let rows = list.iter().rev().map(|job| {
            let id = job.id;
            let chosen = self.jobs.selected == Some(id);
            let dot = if job.running {
                cx.theme().green
            } else if job.failed() {
                cx.theme().danger
            } else {
                cx.theme().muted_foreground
            };
            let by = if job.yours { "yours" } else { "the agent's" };
            h_flex()
                .id(("job-row", id as usize))
                .px_3()
                .py_1p5()
                .gap_2()
                .items_center()
                .cursor_pointer()
                .when(chosen, |row| row.bg(cx.theme().list_active))
                .child(div().flex_shrink_0().size(px(6.)).rounded_full().bg(dot))
                .child(
                    v_flex()
                        .min_w_0()
                        .child(
                            div()
                                .text_xs()
                                .font_family(MONO_FONT)
                                .truncate()
                                .child(letronna_core::tools::one_line(&job.command)),
                        )
                        .child(
                            div()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .truncate()
                                .child(format!("#{id} · {} · {by}", job.state())),
                        ),
                )
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.choose_job(id, cx);
                    this.focus_jobs(window);
                }))
        });
        let empty = list.is_empty().then(|| {
            div()
                .px_3()
                .py_2()
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .child("No background jobs yet. The agent's show here, and so do yours.")
        });
        div()
            .id("job-list")
            .track_focus(&self.jobs.list_focus)
            .key_context("JobList")
            .flex_shrink_0()
            .max_h(LIST_MAX)
            .overflow_y_scroll()
            .border_b_1()
            .border_color(if focused {
                cx.theme().ring
            } else {
                cx.theme().border
            })
            .children(rows)
            .children(empty)
    }

    /// The chosen job's actions over its output.
    fn job_output(&self, job: JobInfo, cx: &mut Context<Self>) -> impl IntoElement {
        let picked = self.jobs.picked;
        let running = job.running;
        let id = job.id;
        // Work on a thread of Letronna's own runs in no folder.
        let terminal = job.cwd.map(|folder| {
            Button::new("job-terminal")
                .framed(cx)
                .xsmall()
                .icon(Icon::new(AppIcon::Terminal))
                .label("Open a terminal here")
                .tooltip(folder.display().to_string())
                .on_click(cx.listener(move |this, _, _, cx| this.open_terminal_in(&folder, cx)))
        });
        let actions = h_flex()
            // The panel is narrower than the three buttons at its usual
            // width, which cut "Open a terminal here" off at the edge.
            .flex_wrap()
            .px_3()
            .py_1p5()
            .gap_1()
            .items_center()
            .border_b_1()
            .border_color(cx.theme().border)
            .child(
                Button::new("stop-job")
                    .framed(cx)
                    .xsmall()
                    .icon(Icon::new(AppIcon::Stop))
                    .label("Stop")
                    .disabled(!running)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if let Err(e) = jobs::stop(id) {
                            this.error = Some(e);
                        }
                        cx.notify();
                    })),
            )
            .child(
                Button::new("add-job-lines")
                    .framed(cx)
                    .xsmall()
                    .icon(Icon::new(AppIcon::MessageSquare))
                    .label("Add selection to message")
                    .disabled(picked.is_none())
                    .tooltip_with_action(
                        "Add the picked lines to your message",
                        &AddLines,
                        Some("JobLog"),
                    )
                    .on_click(cx.listener(|this, _, window, cx| this.add_lines(window, cx))),
            )
            .children(terminal);
        let lines = self.jobs.lines.iter().enumerate().map(|(n, line)| {
            let chosen = picked.is_some_and(|picked| picked.range().contains(&n));
            div()
                .id(("job-line", n))
                .px_3()
                .whitespace_normal()
                .when(chosen, |row| row.bg(cx.theme().selection))
                // A line with no text would draw no height.
                .child(if line.is_empty() {
                    SharedString::from(" ")
                } else {
                    line.clone()
                })
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                        this.pick_line(n, event.modifiers.shift, window, cx)
                    }),
                )
        });
        let note = if picked.is_some() {
            Some("Held while lines are picked. Esc lets go and follows it again.")
        } else if self.jobs.lines.is_empty() {
            Some("Nothing printed yet.")
        } else {
            None
        };
        v_flex()
            .flex_1()
            .min_h_0()
            .child(actions)
            .child(
                div()
                    .id("job-log")
                    .track_focus(&self.jobs.log_focus)
                    .key_context("JobLog")
                    .flex_1()
                    .min_h_0()
                    .py_1()
                    .overflow_y_scroll()
                    .track_scroll(&self.jobs.scroll)
                    .font_family(MONO_FONT)
                    .text_xs()
                    .children(lines)
                    .on_action(
                        cx.listener(|this, _: &LineDown, _, cx| this.pick_step(1, false, cx)),
                    )
                    .on_action(cx.listener(|this, _: &LineUp, _, cx| this.pick_step(-1, false, cx)))
                    .on_action(cx.listener(|this, _: &PickDown, _, cx| this.pick_step(1, true, cx)))
                    .on_action(cx.listener(|this, _: &PickUp, _, cx| this.pick_step(-1, true, cx)))
                    .on_action(cx.listener(|this, _: &PickAll, _, cx| {
                        let len = this.jobs.lines.len();
                        if len > 0 {
                            this.jobs.picked = Some(Picked {
                                anchor: 0,
                                cursor: len - 1,
                            });
                            cx.notify();
                        }
                    }))
                    .on_action(cx.listener(|this, _: &Unpick, _, cx| {
                        // With nothing picked, Escape does what it does in the chat.
                        if this.jobs.picked.is_none() {
                            cx.propagate();
                        } else {
                            this.unpick(cx);
                        }
                    }))
                    .on_action(
                        cx.listener(|this, _: &AddLines, window, cx| this.add_lines(window, cx)),
                    ),
            )
            .children(note.map(|note| {
                div()
                    .px_3()
                    .py_1()
                    .border_t_1()
                    .border_color(cx.theme().border)
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(note)
            }))
    }
}

#[cfg(test)]
mod tests {
    // Not `use super::*`: the prelude re-exports gpui, whose `test` macro
    // would shadow the built-in attribute.
    use super::{MAX_LINES, Picked, fenced, picked_text, tail_lines};
    use gpui::SharedString;

    #[test]
    fn the_keys_move_a_pick_and_shift_stretches_it_within_the_output() {
        let start = Picked::one(5);
        assert_eq!(start.moved(1, false, 10), Picked::one(6));
        let stretched = start.moved(-2, true, 10);
        assert_eq!(stretched.range(), 3..=5);
        // It stops at either end of the output.
        assert_eq!(stretched.moved(-9, true, 10).range(), 0..=5);
        assert_eq!(start.moved(20, false, 10), Picked::one(9));
    }

    #[test]
    fn picked_lines_come_out_as_one_text_in_order() {
        let lines: Vec<SharedString> = ["one", "two", "three"].map(SharedString::from).to_vec();
        let backwards = Picked {
            anchor: 2,
            cursor: 1,
        };
        assert_eq!(
            picked_text(&lines, Some(backwards)).as_deref(),
            Some("two\nthree")
        );
        assert_eq!(picked_text(&lines, None), None);
        // A pick past what is left of the output gives nothing.
        assert_eq!(picked_text(&lines, Some(Picked::one(7))), None);
    }

    #[test]
    fn only_the_newest_lines_of_a_huge_log_are_kept() {
        let log = "\n".repeat(30_000) + "last\n";
        let lines = tail_lines(&log);
        assert_eq!(lines.len(), MAX_LINES);
        assert_eq!(lines.last().map(|line| line.as_ref()), Some("last"));
        assert!(tail_lines("").is_empty());
    }

    #[test]
    fn output_holding_a_fence_is_fenced_by_a_longer_one() {
        assert_eq!(fenced("ok"), "```\nok\n```");
        assert_eq!(fenced("a\n```\nb"), "````\na\n```\nb\n````");
    }
}
