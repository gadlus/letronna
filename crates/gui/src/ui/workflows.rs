//! The Workflows page: the session's workflow runs, each open as a tree of
//! its phases and agents, with the pause, resume, stop, skip and retry core
//! gives them.
//!
//! Core keeps a run's board behind a lock and tells nobody when one of its
//! agents moves on, so the page reads the board as it draws and is drawn
//! again only when something happens: a notice arrives (a run ended or held
//! itself), a turn streams, or a button is pressed, Refresh among them.
//! Nothing ticks while a run works unwatched, so a row can lag until the
//! next of those. Core keeps no one agent's transcript or answer, and no log
//! of a run, where the window can read them, so Open shows the run's result
//! and the page has no log.

use crate::prelude::*;
use letronna_core::agents::took;
use letronna_core::budget::short;
use letronna_core::inbox::{self, Notice, When};
use letronna_core::tools::{Tool, ToolContext};
use letronna_core::workflow::{self, AgentState, RunState, RunView};
use serde_json::{Value, json};

actions!(workflows, [Refresh, Back]);

/// What the Workflows page holds of its own; the runs are core's.
pub(crate) struct Workflows {
    /// The run shown, by name; none shows the list.
    open: Option<String>,
    /// Why core refused the last thing asked of it, until the next ask.
    error: Option<String>,
    /// The open run's result, once asked for.
    result: Option<String>,
    focus: FocusHandle,
}

impl Workflows {
    pub(crate) fn new(cx: &mut App) -> Self {
        Self {
            open: None,
            error: None,
            result: None,
            focus: cx.focus_handle(),
        }
    }
}

/// A run as the page draws it.
#[derive(Debug, PartialEq)]
struct Model {
    name: String,
    title: String,
    /// "running · 1m02s · 3 of 9 done".
    standing: String,
    /// "1.2M of 3.0M tokens".
    spend: String,
    /// The spend as a share of the budget, from 0 to 100, for the bar.
    share: f32,
    phases: Vec<Phase>,
    pause: bool,
    resume: bool,
    stop: bool,
    result: bool,
    /// Said while an agent set to retry waits on a run still going, which
    /// only a stop and a resume run again.
    hint: Option<&'static str>,
}

/// One phase of a run and its agents' rows.
#[derive(Debug, PartialEq)]
struct Phase {
    /// Empty for agents called before any phase was named.
    title: String,
    rows: Vec<Row>,
}

/// One agent as its row shows it, with the buttons its state allows.
#[derive(Debug, PartialEq)]
struct Row {
    number: usize,
    label: String,
    state: &'static str,
    /// "-" for none, as a replayed agent spent nothing in this launch.
    tokens: String,
    skip: bool,
    retry: bool,
}

impl Model {
    fn of(view: &RunView) -> Self {
        let going = matches!(view.state, RunState::Running | RunState::Paused);
        let finished = view.state == RunState::Finished;
        let phases: Vec<Phase> = (view.phases.iter())
            .map(|phase| Phase {
                title: phase.title.clone(),
                rows: (phase.agents.iter())
                    .map(|agent| Row {
                        number: agent.number,
                        label: agent.label.clone(),
                        state: agent.state.word(),
                        tokens: match agent.tokens {
                            0 => "-".to_owned(),
                            tokens => short(tokens),
                        },
                        skip: going
                            && matches!(agent.state, AgentState::Waiting | AgentState::Running),
                        retry: !finished
                            && matches!(
                                agent.state,
                                AgentState::Failed | AgentState::Stopped | AgentState::Skipped
                            ),
                    })
                    .collect(),
            })
            .collect();
        let waiting_retry = (phases.iter().flat_map(|phase| &phase.rows))
            .any(|row| row.state == AgentState::Retry.word());
        Self {
            name: view.name.clone(),
            title: view.title.clone(),
            standing: standing(view),
            spend: format!("{} of {} tokens", short(view.spent), short(view.budget)),
            share: match view.budget {
                0 => 0.,
                budget => (view.spent as f64 * 100. / budget as f64).min(100.) as f32,
            },
            phases,
            pause: view.state == RunState::Running,
            resume: matches!(view.state, RunState::Paused | RunState::Ended(_)),
            stop: going,
            result: finished,
            hint: (going && waiting_retry)
                .then_some("Stop, then Resume, to run the agents set to retry."),
        }
    }
}

/// How a run stands and for how long, with how far it has got while it
/// goes: "running · 1m02s · 3 of 9 done", "stopped · 9s".
fn standing(view: &RunView) -> String {
    let agents = view.phases.iter().flat_map(|phase| &phase.agents);
    let (calls, done) = agents.fold((0, 0), |(calls, done), agent| {
        (calls + 1, done + usize::from(agent.state.answered()))
    });
    let took = took(view.seconds);
    match &view.state {
        RunState::Running => format!("running · {took} · {done} of {calls} done"),
        RunState::Paused => format!("paused · {took} · {done} of {calls} done"),
        RunState::Finished => format!("finished · {took}"),
        RunState::Ended(how) => format!("{how} · {took}"),
    }
}

/// What the `agents` tool answers `args` with for session `session`'s own
/// agent, which is how a run is stopped or its result read.
fn agents_call(session: u64, args: Value) -> Result<String, String> {
    let connection = Connection {
        conversation: session,
        ..Connection::default()
    };
    letronna_core::agents::Agents.run(&args, &ToolContext::of(&connection))
}

/// Picks session `session`'s run `name` back up: a paused one goes on where
/// it is, and one that ended goes to the session's agent as a notice, which
/// shows its launch card again, its finished agents' answers coming back at
/// no cost.
fn resume_run(session: u64, name: &str) -> Result<(), String> {
    let view = workflow::run(session, name)?;
    let RunState::Ended(how) = &view.state else {
        return workflow::resume(session, name);
    };
    letronna_core::crew::grant(session);
    let call = json!({"resume": name});
    let text = format!(
        "The user chose to resume workflow {name} \"{}\", which ended ({how}): call \
         workflow {call}.",
        view.title
    );
    let notice = Notice {
        from: "Crew".into(),
        when: When::Next,
        text,
    };
    inbox::push(session, notice);
    Ok(())
}

/// What a button asks of a run, or of its agent by number.
#[derive(Clone, Copy)]
enum Ask {
    Pause,
    Resume,
    Stop,
    Result,
    Skip(usize),
    Retry(usize),
}

impl Chat {
    /// Shows the Workflows page's list, with the keys the page takes.
    pub(crate) fn open_workflows(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.page = Page::Workflows;
        self.show_run(None, window, cx);
    }

    /// Opens run `name`, or the list for none, with no error or result left
    /// from the one before, and the page's keys working: the button pressed
    /// to get here, which had focus, is gone.
    fn show_run(&mut self, name: Option<String>, window: &mut Window, cx: &mut Context<Self>) {
        self.workflows.open = name;
        self.workflows.error = None;
        self.workflows.result = None;
        window.focus(&self.workflows.focus);
        cx.notify();
    }

    /// Draws the page again, which reads the runs again, as a person asks.
    fn refresh_workflows(&mut self, cx: &mut Context<Self>) {
        self.workflows.error = None;
        cx.notify();
    }

    /// Asks core for `ask` on run `name` of the session on screen, showing a
    /// refusal as the page's error line.
    fn ask_run(&mut self, name: &str, ask: Ask, cx: &mut Context<Self>) {
        let session = self.session_on_screen();
        // A run that ended is resumed by the session's agent, whose launch
        // card waits for a yes in the chat, so the chat is where to be.
        let to_chat = matches!(ask, Ask::Resume)
            && workflow::run(session, name)
                .is_ok_and(|view| matches!(view.state, RunState::Ended(_)));
        match asked(session, name, ask) {
            Ok(result) => {
                self.workflows.error = None;
                if result.is_some() {
                    self.workflows.result = result;
                }
                if to_chat {
                    self.page = Page::Chat;
                }
            }
            Err(error) => self.workflows.error = Some(error),
        }
        cx.notify();
    }
}

/// What core makes of `ask` on session `session`'s run `name`: the run's
/// result when that is what was asked, or why it refused.
fn asked(session: u64, name: &str, ask: Ask) -> Result<Option<String>, String> {
    let agent = |number: usize| format!("#{number}");
    match ask {
        Ask::Pause => workflow::pause(session, name).map(|()| None),
        Ask::Resume => resume_run(session, name).map(|()| None),
        Ask::Stop => agents_call(session, json!({"action": "stop", "to": name})).map(|_| None),
        Ask::Result => agents_call(session, json!({"action": "report", "to": name})).map(Some),
        Ask::Skip(number) => workflow::skip(session, name, &agent(number)).map(|()| None),
        Ask::Retry(number) => workflow::retry(session, name, &agent(number)).map(|()| None),
    }
}

impl Chat {
    /// The Workflows page: the open run of the session on screen, or all of
    /// its runs when none is open or the open one is not its.
    pub(crate) fn workflows_page(
        &self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let runs = workflow::runs(self.session_on_screen());
        let open = (self.workflows.open.as_ref())
            .and_then(|name| runs.iter().find(|run| &run.name == name));
        let body = match open {
            Some(view) => self
                .run_page(&Model::of(view), window, cx)
                .into_any_element(),
            None => self.run_list(&runs, cx).into_any_element(),
        };
        v_flex()
            .key_context("Workflows")
            .track_focus(&self.workflows.focus)
            .on_action(cx.listener(|this, _: &Refresh, _, cx| this.refresh_workflows(cx)))
            .on_action(cx.listener(|this, _: &Back, window, cx| {
                // On the list there is nowhere to go back to, so Escape does
                // what it does in the chat.
                if this.workflows.open.is_none() {
                    cx.propagate();
                    return;
                }
                this.show_run(None, window, cx);
            }))
            .flex_1()
            .min_h_0()
            .px_8()
            .py_6()
            .gap_4()
            .child(body)
    }

    /// A page's heading, its line under it, and the buttons beside them.
    fn workflows_header(
        &self,
        title: String,
        blurb: String,
        buttons: Vec<AnyElement>,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let refresh = Button::new("workflows-refresh")
            .ghost()
            .small()
            .icon(Icon::new(AppIcon::Reload))
            .tooltip("Refresh")
            .on_click(cx.listener(|this, _, _, cx| this.refresh_workflows(cx)));
        v_flex()
            .gap_1()
            .child(
                h_flex()
                    .items_center()
                    .justify_between()
                    .gap_4()
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_xl()
                            .font_family(HEADING_FONT)
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(title),
                    )
                    .child(
                        h_flex()
                            .gap_1()
                            .items_center()
                            .children(buttons)
                            .child(refresh),
                    ),
            )
            .child(
                div()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child(blurb),
            )
            .when_some(self.workflows.error.clone(), |this, error| {
                this.child(div().text_sm().text_color(cx.theme().danger).child(error))
            })
    }

    /// Every run of the session on screen, oldest first, each one row.
    fn run_list(&self, runs: &[RunView], cx: &mut Context<Self>) -> impl IntoElement {
        let blurb = if runs.is_empty() {
            "No workflow runs in this session yet. When Letronna runs one, once you \
             approve it, it shows here."
        } else {
            "The runs of helpers Letronna started in this session since it opened."
        };
        let rows: Vec<AnyElement> = (runs.iter().enumerate())
            .map(|(index, view)| {
                let model = Model::of(view);
                let name = model.name.clone();
                h_flex()
                    .w_full()
                    .items_center()
                    .gap_3()
                    .p_3()
                    .rounded(cx.theme().radius)
                    .border_1()
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
                                    .child(model.title),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(cx.theme().muted_foreground)
                                    .truncate()
                                    .child(format!(
                                        "{} · {} · {}",
                                        model.name, model.standing, model.spend
                                    )),
                            ),
                    )
                    .child(
                        Button::new(("open-run", index))
                            .ghost()
                            .xsmall()
                            .label("Open")
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.show_run(Some(name.clone()), window, cx)
                            })),
                    )
                    .into_any_element()
            })
            .collect();
        v_flex()
            .flex_1()
            .min_h_0()
            .gap_4()
            .child(self.workflows_header("Workflows".into(), blurb.into(), Vec::new(), cx))
            .child(
                div()
                    .id("workflow-runs")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .child(v_flex().gap_2().children(rows)),
            )
    }

    /// A run's own button, which asks core for `ask` when it is `on`.
    fn run_button(
        &self,
        name: &str,
        ask: Ask,
        icon: AppIcon,
        label: &'static str,
        on: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let name = name.to_owned();
        Button::new(ElementId::Name(format!("run-{label}").into()))
            .ghost()
            .small()
            .icon(Icon::new(icon))
            .label(label)
            .disabled(!on)
            .on_click(cx.listener(move |this, _, _, cx| this.ask_run(&name, ask, cx)))
            .into_any_element()
    }

    /// One run: its buttons, its spend against its budget, and its phases
    /// with a row for each agent, then its result once asked for.
    fn run_page(
        &self,
        model: &Model,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let name = model.name.as_str();
        let back = Button::new("workflow-back")
            .ghost()
            .small()
            .icon(Icon::new(IconName::ArrowLeft))
            .tooltip("All runs")
            .on_click(cx.listener(|this, _, window, cx| this.show_run(None, window, cx)))
            .into_any_element();
        let buttons = vec![
            back,
            self.run_button(name, Ask::Pause, AppIcon::Pause, "Pause", model.pause, cx),
            self.run_button(name, Ask::Resume, AppIcon::Play, "Resume", model.resume, cx),
            self.run_button(name, Ask::Stop, AppIcon::Stop, "Stop", model.stop, cx),
            self.run_button(
                name,
                Ask::Result,
                AppIcon::FileText,
                "Result",
                model.result,
                cx,
            ),
        ];
        let blurb = format!("{} · {}", model.name, model.standing);
        let tree: Vec<AnyElement> = (model.phases.iter())
            .map(|phase| {
                let rows: Vec<AnyElement> = (phase.rows.iter())
                    .map(|row| self.agent_row(name, row, cx))
                    .collect();
                v_flex()
                    .gap_1()
                    .when(!phase.title.is_empty(), |this| {
                        this.child(
                            div()
                                .text_sm()
                                .font_family(HEADING_FONT)
                                .font_weight(FontWeight::MEDIUM)
                                .child(phase.title.clone()),
                        )
                    })
                    .child(
                        v_flex()
                            .ml_1()
                            .pl_3()
                            .border_l_1()
                            .border_color(cx.theme().border)
                            .children(rows),
                    )
                    .into_any_element()
            })
            .collect();
        v_flex()
            .flex_1()
            .min_h_0()
            .gap_4()
            .child(self.workflows_header(model.title.clone(), blurb, buttons, cx))
            .child(
                v_flex()
                    .gap_1()
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(model.spend.clone()),
                    )
                    .child(
                        gpui_component::progress::Progress::new()
                            .h(px(4.))
                            .value(model.share),
                    )
                    .when_some(model.hint, |this, hint| {
                        this.child(
                            div()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child(hint),
                        )
                    }),
            )
            .child(
                div()
                    .id("workflow-tree")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .child(v_flex().gap_3().children(tree).when_some(
                        self.workflows.result.clone(),
                        |this, result| {
                            this.child(crate::ui::code_block(
                                "workflow-result",
                                result,
                                px(320.),
                                window,
                                cx,
                            ))
                        },
                    )),
            )
    }

    /// One agent of run `name`: its number, label, state and spend, and the
    /// skip and retry its state allows.
    fn agent_row(&self, name: &str, row: &Row, cx: &mut Context<Self>) -> AnyElement {
        let number = row.number;
        let (skip, retry) = (name.to_owned(), name.to_owned());
        let muted = cx.theme().muted_foreground;
        h_flex()
            .h(px(28.))
            .items_center()
            .gap_3()
            .text_sm()
            .child(
                div()
                    .w(px(36.))
                    .flex_shrink_0()
                    .text_xs()
                    .font_family(MONO_FONT)
                    .text_color(muted)
                    .child(format!("#{number}")),
            )
            .child(div().flex_1().min_w_0().truncate().child(row.label.clone()))
            .child(
                div()
                    .w(px(64.))
                    .flex_shrink_0()
                    .text_xs()
                    .text_color(muted)
                    .child(row.state),
            )
            .child(
                div()
                    .w(px(44.))
                    .flex_shrink_0()
                    .text_xs()
                    .text_right()
                    .text_color(muted)
                    .child(row.tokens.clone()),
            )
            .child(
                Button::new(("skip-agent", number))
                    .ghost()
                    .xsmall()
                    .icon(Icon::new(AppIcon::Skip))
                    .label("Skip")
                    .disabled(!row.skip)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.ask_run(&skip, Ask::Skip(number), cx)
                    })),
            )
            .child(
                Button::new(("retry-agent", number))
                    .ghost()
                    .xsmall()
                    .icon(Icon::new(AppIcon::Reload))
                    .label("Retry")
                    .disabled(!row.retry)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.ask_run(&retry, Ask::Retry(number), cx)
                    })),
            )
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    // Not `use super::*`: the prelude re-exports gpui, whose `test` macro
    // would shadow the built-in attribute.
    use super::{Ask, Model, asked};
    use letronna_core::workflow::{AgentState, AgentView, PhaseView, RunState, RunView};

    fn agent(number: usize, phase: &str, state: AgentState, tokens: u64) -> AgentView {
        AgentView {
            number,
            label: format!("wf-3 #{number}"),
            phase: phase.into(),
            state,
            tokens,
        }
    }

    /// Run wf-3 in `state`, a first phase done and a second at work.
    fn run(state: RunState) -> RunView {
        RunView {
            name: "wf-3".into(),
            title: "Review".into(),
            state,
            phases: vec![
                PhaseView {
                    title: "Find".into(),
                    agents: vec![
                        agent(1, "Find", AgentState::Done, 41_000),
                        agent(2, "Find", AgentState::Replayed, 0),
                    ],
                },
                PhaseView {
                    title: "Verify".into(),
                    agents: vec![
                        agent(3, "Verify", AgentState::Running, 900),
                        agent(4, "Verify", AgentState::Waiting, 0),
                        agent(5, "Verify", AgentState::Failed, 2_500),
                        agent(6, "Verify", AgentState::Skipped, 0),
                    ],
                },
            ],
            spent: 1_250_000,
            budget: 3_000_000,
            seconds: 62,
        }
    }

    /// Each row's number, state, spend, and whether it offers skip and retry.
    fn rows(model: &Model) -> Vec<(usize, &str, &str, bool, bool)> {
        let rows = model.phases.iter().flat_map(|phase| &phase.rows);
        rows.map(|row| {
            (
                row.number,
                row.state,
                row.tokens.as_str(),
                row.skip,
                row.retry,
            )
        })
        .collect()
    }

    #[test]
    fn a_run_shows_its_phases_with_a_row_per_agent_and_its_spend() {
        let model = Model::of(&run(RunState::Running));
        let titles: Vec<&str> = model.phases.iter().map(|p| p.title.as_str()).collect();
        assert_eq!(titles, ["Find", "Verify"]);
        assert_eq!(model.phases[1].rows[0].label, "wf-3 #3");
        assert_eq!(model.standing, "running · 1m02s · 2 of 6 done");
        assert_eq!(model.spend, "1.2M of 3.0M tokens");
        assert!((model.share - 41.666).abs() < 0.01);
        let states: Vec<(usize, &str, &str)> = (rows(&model).into_iter())
            .map(|(number, state, tokens, ..)| (number, state, tokens))
            .collect();
        assert_eq!(
            states,
            [
                (1, "done", "41k"),
                (2, "replayed", "-"),
                (3, "running", "900"),
                (4, "waiting", "-"),
                (5, "failed", "2k"),
                (6, "skipped", "-"),
            ]
        );
    }

    #[test]
    fn a_run_over_its_budget_fills_the_bar_and_one_with_none_leaves_it_empty() {
        let mut view = run(RunState::Running);
        view.spent = 4_000_000;
        assert_eq!(Model::of(&view).share, 100.);
        view.budget = 0;
        assert_eq!(Model::of(&view).share, 0.);
    }

    /// Which of pause, resume, stop and result a run in `state` offers.
    fn buttons(state: RunState) -> [bool; 4] {
        let model = Model::of(&run(state));
        [model.pause, model.resume, model.stop, model.result]
    }

    #[test]
    fn each_run_state_offers_only_the_buttons_that_work_in_it() {
        assert_eq!(buttons(RunState::Running), [true, false, true, false]);
        assert_eq!(buttons(RunState::Paused), [false, true, true, false]);
        assert_eq!(buttons(RunState::Finished), [false, false, false, true]);
        let stopped = RunState::Ended("stopped".into());
        assert_eq!(buttons(stopped), [false, true, false, false]);
    }

    #[test]
    fn skip_is_offered_to_waiting_and_running_agents_of_a_run_still_going() {
        for state in [RunState::Running, RunState::Paused] {
            let model = Model::of(&run(state));
            let skips: Vec<usize> = (rows(&model).into_iter())
                .filter_map(|(number, _, _, skip, _)| skip.then_some(number))
                .collect();
            assert_eq!(skips, [3, 4]);
        }
        let ended = Model::of(&run(RunState::Ended("failed".into())));
        assert!(rows(&ended).iter().all(|row| !row.3));
    }

    #[test]
    fn retry_is_offered_to_failed_stopped_and_skipped_agents_until_the_run_finishes() {
        for state in [RunState::Running, RunState::Ended("stopped".into())] {
            let model = Model::of(&run(state));
            let retries: Vec<usize> = (rows(&model).into_iter())
                .filter_map(|(number, _, _, _, retry)| retry.then_some(number))
                .collect();
            assert_eq!(retries, [5, 6]);
        }
        let finished = Model::of(&run(RunState::Finished));
        assert!(rows(&finished).iter().all(|row| !row.4));
    }

    #[test]
    fn an_agent_set_to_retry_in_a_run_still_going_says_how_to_run_it() {
        let mut view = run(RunState::Running);
        assert_eq!(Model::of(&view).hint, None);
        view.phases[1].agents[2].state = AgentState::Retry;
        let hint = Model::of(&view).hint;
        assert_eq!(
            hint,
            Some("Stop, then Resume, to run the agents set to retry.")
        );
        view.state = RunState::Ended("stopped".into());
        assert_eq!(Model::of(&view).hint, None);
    }

    #[test]
    fn a_control_core_refuses_comes_back_as_its_readable_refusal() {
        // No session has a run by this name, and it is no run's folder name,
        // so core refuses without reading the disk.
        let refused = "No workflow run called nothing-here.";
        for ask in [Ask::Pause, Ask::Resume, Ask::Skip(1), Ask::Retry(1)] {
            assert_eq!(asked(u64::MAX, "nothing-here", ask), Err(refused.into()));
        }
    }
}
