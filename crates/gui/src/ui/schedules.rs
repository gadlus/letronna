//! The Scheduled jobs page: prompts that run on a timer.

use crate::prelude::*;
use letronna_core::gate::Mode;
use letronna_core::schedule::{self, Schedule, Trigger};

/// The intervals offered for "every so often". A free-form number would need
/// validation and buys little; these are the ones people actually pick.
const INTERVALS: &[u32] = &[15, 30, 60, 180, 360, 720];

/// The modes a job can run in, as the editor names them. Don't ask is left
/// out: with nobody there to answer, Default already refuses what would ask.
const MODES: [(Mode, &str); 5] = [
    (Mode::Default, "Default"),
    (Mode::AcceptEdits, "Accept edits"),
    (Mode::Plan, "Plan"),
    (Mode::Auto, "Auto"),
    (Mode::Bypass, "Bypass"),
];

impl Chat {
    /// The Scheduled jobs page, its list read again: the model's `schedule`
    /// tool, or another Letronna, may have changed it since the last read.
    pub(crate) fn open_jobs(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.schedules = self.schedule_runner.list();
        self.page = Page::Jobs;
        self.focus_composer(window, cx);
        cx.notify();
    }

    /// Change the schedules through the runner, which reads them afresh and
    /// saves at once, so a run's `last_run` is never written back over.
    fn change_schedules(&mut self, edit: impl FnOnce(&mut Vec<Schedule>), cx: &mut Context<Self>) {
        if let Err(error) = self.schedule_runner.change(edit) {
            self.error = Some(error);
        }
        self.schedules = self.schedule_runner.list();
        cx.notify();
    }

    /// Change job `id`, if it is still there.
    fn change_schedule(
        &mut self,
        id: u64,
        edit: impl FnOnce(&mut Schedule),
        cx: &mut Context<Self>,
    ) {
        self.change_schedules(
            |list| {
                if let Some(job) = list.iter_mut().find(|s| s.id == id) {
                    edit(job);
                }
            },
            cx,
        );
    }

    pub(crate) fn new_schedule(&mut self, cx: &mut Context<Self>) {
        let mut fresh = Schedule {
            id: 0,
            name: "New job".into(),
            prompt: String::new(),
            workspace: self.store.conversations[self.store.active]
                .workspace
                .clone(),
            trigger: Trigger::DailyAt {
                hour: 9,
                minute: 0,
                weekdays_only: true,
            },
            enabled: false,
            last_run: 0,
            mode: Mode::Default,
            by_model: false,
        };
        schedule::armed_now(&mut fresh);
        let mut id = 0;
        self.change_schedules(
            |list| {
                // Numbered against the list on disk, which may have grown
                // since this page last read it.
                fresh.id = schedule::next_id(list);
                id = fresh.id;
                list.push(fresh);
            },
            cx,
        );
        self.editing = Some(id);
    }

    /// The editor for one job, shown in place of its row.
    fn schedule_editor(&self, index: usize, cx: &mut Context<Self>) -> impl IntoElement {
        let job = &self.schedules[index];
        let id = job.id;
        let trigger = job.trigger.clone();
        // Don't ask, set in the file by hand, refuses what would ask as a
        // run in Default does with nobody there, so it shows as Default.
        let mode = match job.mode {
            Mode::DontAsk => Mode::Default,
            mode => mode,
        };
        let modes: Vec<_> = MODES
            .iter()
            .enumerate()
            .map(|(ix, &(choice, name))| {
                Button::new(("mode", ix))
                    .xsmall()
                    .when(choice == mode, |b| b.primary())
                    .when(choice != mode, |b| b.framed(cx))
                    .label(name)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.change_schedule(id, |job| job.mode = choice, cx)
                    }))
            })
            .collect();

        let every: Vec<_> = INTERVALS
            .iter()
            .map(|minutes| {
                let minutes = *minutes;
                let picked = matches!(trigger, Trigger::Every { minutes: m } if m == minutes);
                Button::new(("every", minutes as usize))
                    .xsmall()
                    .when(picked, |b| b.primary())
                    .when(!picked, |b| b.framed(cx))
                    .label(Trigger::Every { minutes }.label())
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.set_trigger(id, Trigger::Every { minutes }, cx)
                    }))
            })
            .collect();

        let hours: Vec<_> = [7, 9, 12, 18, 21].iter().map(|hour| {
            let hour = *hour;
            let weekdays_only = matches!(trigger, Trigger::DailyAt { weekdays_only, .. } if weekdays_only)
                || !matches!(trigger, Trigger::DailyAt { .. });
            let picked = matches!(trigger, Trigger::DailyAt { hour: h, .. } if h == hour);
            Button::new(("at", hour as usize))
                .xsmall()
                .when(picked, |b| b.primary())
                .when(!picked, |b| b.framed(cx))
                .label(format!("{hour:02}:00"))
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.set_trigger(
                        id,
                        Trigger::DailyAt {
                            hour,
                            minute: 0,
                            weekdays_only,
                        },
                        cx,
                    )
                }))
        }).collect();

        let weekdays_only = matches!(
            trigger,
            Trigger::DailyAt {
                weekdays_only: true,
                ..
            }
        );
        let is_daily = matches!(trigger, Trigger::DailyAt { .. });

        v_flex()
            .w_full()
            // Clear of the hairlines of the rows either side.
            .my_2()
            .gap_3()
            .p_3()
            .rounded(cx.theme().radius)
            .border_1()
            .border_color(cx.theme().primary.opacity(0.4))
            .bg(cx.theme().secondary.opacity(0.3))
            .child(field(
                "Name",
                Input::new(&self.job_name).small().w_full(),
                cx,
            ))
            .child(field(
                "Prompt to send",
                Input::new(&self.job_prompt).small().w_full(),
                cx,
            ))
            .child(
                v_flex()
                    .gap_1p5()
                    .child(label("At a time of day", cx))
                    .child(
                        h_flex().flex_wrap().gap_2().children(hours).child(
                            Button::new("weekdays")
                                .xsmall()
                                .when(weekdays_only && is_daily, |b| b.primary())
                                .when(!(weekdays_only && is_daily), |b| b.framed(cx))
                                .label("Weekdays only")
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    if let Trigger::DailyAt {
                                        hour,
                                        minute,
                                        weekdays_only,
                                    } = trigger
                                    {
                                        this.set_trigger(
                                            id,
                                            Trigger::DailyAt {
                                                hour,
                                                minute,
                                                weekdays_only: !weekdays_only,
                                            },
                                            cx,
                                        );
                                    }
                                })),
                        ),
                    ),
            )
            .child(
                v_flex()
                    .gap_1p5()
                    .child(label("Or on an interval", cx))
                    .child(h_flex().flex_wrap().gap_2().children(every)),
            )
            .child(
                v_flex()
                    .gap_1p5()
                    .child(label("Mode", cx))
                    .child(h_flex().flex_wrap().gap_2().children(modes))
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(about(mode)),
                    ),
            )
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        Button::new("save-job")
                            .primary()
                            .small()
                            .label("Save")
                            .on_click(
                                cx.listener(move |this, _, _, cx| this.commit_schedule(id, cx)),
                            ),
                    )
                    .child(
                        Button::new("cancel-job")
                            .plain(cx)
                            .small()
                            .label("Cancel")
                            .on_click(cx.listener(move |this, _, _, cx| {
                                // A job that was never given a prompt was never
                                // really created; do not leave the row behind.
                                this.editing = None;
                                this.change_schedules(
                                    |list| list.retain(|s| s.id != id || !s.prompt.is_empty()),
                                    cx,
                                );
                            })),
                    ),
            )
    }

    pub(crate) fn set_trigger(&mut self, id: u64, trigger: Trigger, cx: &mut Context<Self>) {
        self.change_schedule(
            id,
            |job| {
                job.trigger = trigger;
                schedule::armed_now(job);
            },
            cx,
        );
    }

    fn commit_schedule(&mut self, id: u64, cx: &mut Context<Self>) {
        let name = self.job_name.read(cx).value().trim().to_owned();
        let prompt = self.job_prompt.read(cx).value().trim().to_owned();
        if prompt.is_empty() {
            self.error = Some("A scheduled job needs a prompt to send.".into());
            cx.notify();
            return;
        }
        self.editing = None;
        self.change_schedule(
            id,
            |job| {
                job.name = if name.is_empty() {
                    prompt.chars().take(40).collect()
                } else {
                    name
                };
                job.prompt = prompt;
            },
            cx,
        );
    }

    fn delete_schedule(&mut self, id: u64, window: &mut Window, cx: &mut Context<Self>) {
        let Some(job) = self.schedules.iter().find(|s| s.id == id) else {
            return;
        };
        let body = format!("\"{}\" will stop running and be removed.", job.name);
        let chat = cx.entity();
        crate::ui::confirm_delete("Delete job", body, window, cx, move |_, cx| {
            chat.update(cx, |this, cx| {
                this.change_schedules(|list| list.retain(|s| s.id != id), cx);
            })
        });
    }

    fn open_editor(&mut self, id: u64, window: &mut Window, cx: &mut Context<Self>) {
        let Some(job) = self.schedules.iter().find(|s| s.id == id) else {
            return;
        };
        let (name, prompt) = (job.name.clone(), job.prompt.clone());
        self.job_name
            .update(cx, |input, cx| input.set_value(name, window, cx));
        self.job_prompt
            .update(cx, |input, cx| input.set_value(prompt, window, cx));
        self.editing = Some(id);
        cx.notify();
    }

    fn schedule_row(&self, index: usize, cx: &mut Context<Self>) -> impl IntoElement {
        let job = &self.schedules[index];
        let id = job.id;
        let enabled = job.enabled;
        let last = if job.last_run == 0 {
            "never run".to_owned()
        } else {
            format!("last run {} ago", age_label(job.last_run, backend::now()))
        };
        let folder = job
            .workspace
            .as_ref()
            .and_then(|p| p.file_name())
            .map(|n| n.to_string_lossy().into_owned());

        h_flex()
            .w_full()
            .items_center()
            .justify_between()
            .gap_3()
            .py_3()
            .border_b_1()
            .border_color(cx.theme().border)
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .gap_0p5()
                    .child(
                        div()
                            .text_sm()
                            .font_weight(FontWeight::MEDIUM)
                            .truncate()
                            .child(job.name.clone()),
                    )
                    .child(
                        h_flex()
                            .gap_2()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(job.trigger.label())
                            .when_some(folder, |this, folder| this.child(format!("· {folder}")))
                            .child(format!("· {last}")),
                    ),
            )
            .child(
                h_flex()
                    .gap_1()
                    .items_center()
                    .child(
                        Button::new(("run", index))
                            .plain(cx)
                            .xsmall()
                            .label("Run now")
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.schedule_runner.run_now(id);
                                this.schedules = this.schedule_runner.list();
                                cx.notify();
                            })),
                    )
                    .child(
                        Button::new(("edit", index))
                            .plain(cx)
                            .xsmall()
                            .icon(Icon::new(AppIcon::Pencil))
                            .tooltip("Edit")
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.open_editor(id, window, cx)
                            })),
                    )
                    .child(
                        Button::new(("delete", index))
                            .plain(cx)
                            .xsmall()
                            .icon(Icon::new(AppIcon::Trash))
                            .tooltip("Delete")
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.delete_schedule(id, window, cx)
                            })),
                    )
                    .child(
                        div().cursor_pointer().child(
                            gpui_component::switch::Switch::new(("on", index))
                                .checked(enabled)
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.change_schedule(
                                        id,
                                        |job| {
                                            job.enabled = !enabled;
                                            schedule::armed_now(job);
                                        },
                                        cx,
                                    );
                                })),
                        ),
                    ),
            )
    }

    pub(crate) fn schedules_page(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let rows: Vec<AnyElement> = (0..self.schedules.len())
            .map(|index| {
                if self.editing == Some(self.schedules[index].id) {
                    self.schedule_editor(index, cx).into_any_element()
                } else {
                    self.schedule_row(index, cx).into_any_element()
                }
            })
            .collect();
        v_flex()
            .flex_1()
            .min_h_0()
            .px_8()
            .py_6()
            .gap_4()
            .child(
                h_flex()
                    .items_center()
                    .justify_between()
                    .gap_4()
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .gap_1()
                            .child(
                                div()
                                    .text_xl()
                                    .font_family(HEADING_FONT)
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child("Scheduled jobs"),
                            )
                            .child(
                                div()
                                    .text_sm()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(if self.schedules.is_empty() {
                                        "A prompt that runs on a timer. Each run opens its own \
                                         session, and works while the window is hidden in the tray."
                                    } else {
                                        "Each run opens its own session, and works while the \
                                         window is hidden in the tray."
                                    }),
                            ),
                    )
                    .child(
                        Button::new("add-job")
                            .primary()
                            .small()
                            .flex_shrink_0()
                            .icon(Icon::new(IconName::Plus))
                            .label("New job")
                            .on_click(cx.listener(|this, _, _, cx| this.new_schedule(cx))),
                    ),
            )
            .child(
                div()
                    .id("schedule-list")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .child(v_flex().children(rows)),
            )
    }
}

/// What a run of a job in `mode` does, where nobody is there to say yes.
fn about(mode: Mode) -> &'static str {
    match mode {
        Mode::Default | Mode::DontAsk => {
            "Runs only what your permission rules allow. Nobody is there to say yes, so \
             anything else is refused and the job says what it left undone."
        }
        Mode::AcceptEdits => {
            "Also edits files in the job's folder. Commands your rules do not allow are \
             refused."
        }
        Mode::Plan => "Reads and searches only: nothing is written or run.",
        Mode::Auto => {
            "Your model judges what your rules do not settle, taking the job's prompt as \
             what you want. Whatever it does not allow is refused."
        }
        Mode::Bypass => {
            "Runs everything except what your rules deny and commands that are hard to \
             undo, such as recursive deletes and force pushes."
        }
    }
}

fn label(text: &'static str, cx: &App) -> impl IntoElement {
    div()
        .text_xs()
        .text_color(cx.theme().muted_foreground)
        .child(text)
}

fn field(text: &'static str, input: impl IntoElement, cx: &App) -> impl IntoElement {
    v_flex().gap_1p5().child(label(text, cx)).child(input)
}
