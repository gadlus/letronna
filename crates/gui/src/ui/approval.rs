//! The card the chat shows when a tool needs the user: approve a write or a
//! command, answer an `ask_user` question, or approve a plan. A helper's
//! call gets a card of its own under the helper's name.

use crate::prelude::*;
use crate::ui::actions::{NextTurn, PreviousTurn, Stop};
use letronna_core::approvals::Decision;
use letronna_core::{gate, tools};

/// Picks a question card's answer by its place, for the letter keys.
#[derive(Clone, PartialEq, Action)]
#[action(namespace = question, no_json)]
pub(crate) struct PickOption(pub(crate) usize);

impl Chat {
    /// The answers offered by the question waiting on its card, if one is.
    pub(crate) fn question_options(&self) -> Option<Vec<String>> {
        let turn = self.turn.as_ref()?;
        let (call, waiting) = front_call(turn, self.asking.as_deref())?;
        if !waiting || call.name != tools::ASK_USER {
            return None;
        }
        let args = tools::parse_args_for(&call.name, &call.arguments).unwrap_or_default();
        Some(options_in(&args))
    }

    /// Answer the question waiting with its answer at `n`, if it has one.
    fn pick_option(&mut self, n: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(pick) = self
            .question_options()
            .and_then(|options| options.into_iter().nth(n))
        else {
            return;
        };
        // What had the keys is about to go with the card.
        self.focus_composer(window, cx);
        self.resolve_call(Verdict::Answer(pick), cx);
    }

    pub(crate) fn submit_answer(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let text = self.answer.read(cx).value().trim().to_owned();
        if text.is_empty() {
            return;
        }
        self.answer
            .update(cx, |input, cx| input.set_value("", window, cx));
        self.resolve_call(Verdict::Answer(text), cx);
    }

    /// Denies the call on the card, with what was typed in its reason field.
    pub(crate) fn deny_call(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let reason = self.take_reason(window, cx);
        self.resolve_call(Verdict::Deny(reason), cx);
    }

    /// Empties the card's reason field and hands back what was in it, so a
    /// reason typed on one card cannot go with the next.
    fn take_reason(&mut self, window: &mut Window, cx: &mut Context<Self>) -> String {
        let reason = self.deny_reason.read(cx).value().to_string();
        self.deny_reason
            .update(cx, |input, cx| input.set_value("", window, cx));
        reason
    }

    /// The row under the messages while tools run: a line per running call,
    /// or the card waiting on the user. `None` when nothing is queued.
    pub(crate) fn approval_card(
        &self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let turn = self.turn.as_ref()?;
        let (call, waiting) = front_call(turn, self.asking.as_deref())?;
        let ctx = tools::ToolContext::of(&turn.connection);
        let summary = tools::summary(&call.name, &call.arguments);
        if !waiting {
            // Calls from one reply that run together get a line each, which
            // goes as its result comes back; one Auto mode's judge is still
            // deciding says so.
            let lines = running_calls(turn, self.outstanding)
                .into_iter()
                .map(|call| {
                    let checking = self.judging.as_ref().is_some_and(|(id, _)| *id == call.id);
                    h_flex()
                        .gap_2()
                        .items_center()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(
                            gpui_component::spinner::Spinner::new()
                                .xsmall()
                                .color(cx.theme().primary),
                        )
                        .child(call.name.clone())
                        .child(
                            div()
                                .font_family(MONO_FONT)
                                .child(tools::summary(&call.name, &call.arguments)),
                        )
                        .when(checking, |row| row.child("checking…"))
                });
            return Some(v_flex().gap_1().children(lines).into_any_element());
        }

        let args = tools::parse_args_for(&call.name, &call.arguments).unwrap_or_default();
        let card = v_flex()
            .w_full()
            .gap_3()
            .p_3()
            .rounded(cx.theme().radius_lg)
            .border_1()
            .border_color(cx.theme().primary.opacity(0.5))
            .bg(cx.theme().secondary.opacity(0.3));

        if call.name == tools::ASK_USER {
            let question = args["question"].as_str().unwrap_or_default().to_owned();
            // Its own focus, beside the answer box rather than round it, so
            // the letters pick here and type there.
            let mut chips = h_flex()
                .id("question")
                .track_focus(&self.question_focus)
                .key_context("Question")
                .on_action(cx.listener(|this, PickOption(n): &PickOption, window, cx| {
                    this.pick_option(*n, window, cx)
                }))
                .on_action(cx.listener(|this, _: &PreviousTurn, window, cx| {
                    this.step_turn(Some(Stop::Question), false, window, cx)
                }))
                .on_action(cx.listener(|this, _: &NextTurn, window, cx| {
                    this.step_turn(Some(Stop::Question), true, window, cx)
                }))
                .flex_wrap()
                .gap_2();
            for (i, option) in options_in(&args).into_iter().enumerate() {
                let pick = option.clone();
                let mut chip = Button::new(("option", i)).framed(cx).small();
                // Its letter, while the keys are on the answers to press it.
                if let Some(letter) =
                    option_letter(i).filter(|_| self.question_focus.is_focused(window))
                {
                    chip = chip.child(
                        Kbd::new(Keystroke {
                            key: letter.to_string(),
                            ..Default::default()
                        })
                        .text_xs(),
                    );
                }
                chips = chips.child(chip.label(option).on_click(cx.listener(
                    move |this, _, _, cx| this.resolve_call(Verdict::Answer(pick.clone()), cx),
                )));
            }
            return Some(
                card.child(div().text_sm().child(question))
                    .child(chips)
                    .child(
                        h_flex()
                            .gap_2()
                            .items_center()
                            .child(
                                div()
                                    .flex_1()
                                    .child(Input::new(&self.answer).small().w_full()),
                            )
                            .child(
                                Button::new("answer")
                                    .primary()
                                    .small()
                                    .label("Reply")
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.submit_answer(window, cx)
                                    })),
                            ),
                    )
                    .into_any_element(),
            );
        }

        if call.name == tools::EXIT_PLAN {
            let plan = tools::preview(&call.name, &call.arguments, &ctx);
            return Some(
                card.child(
                    div()
                        .text_sm()
                        .font_family(BODY_FONT)
                        .font_weight(FontWeight::MEDIUM)
                        .child("Letronna has a plan"),
                )
                .child(
                    // Clipped as a reply is: the markdown renderer can report
                    // a width wider than the card.
                    div().w_full().overflow_hidden().child(TextView::markdown(
                        SharedString::from(format!("plan-{}", call.id)),
                        plan,
                        window,
                        cx,
                    )),
                )
                .child(
                    h_flex()
                        .gap_2()
                        .child(
                            Button::new("approve-plan")
                                .primary()
                                .small()
                                .label("Approve and carry on")
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.resolve_call(Verdict::Allow { remember: false }, cx)
                                })),
                        )
                        .child(
                            Button::new("keep-planning")
                                .outline()
                                .small()
                                .label("Keep planning")
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.resolve_call(Verdict::Deny(String::new()), cx)
                                })),
                        ),
                )
                .into_any_element(),
            );
        }

        let title = match call.name.as_str() {
            "write_file" => format!("Letronna wants to write {summary}"),
            "edit_file" => format!("Letronna wants to edit {summary}"),
            "run_command" => "Letronna wants to run a command".to_owned(),
            "browse" | "use_page" => format!("Letronna wants to {summary}"),
            "search_web" => "Letronna wants to search the web".to_owned(),
            other => format!("Letronna wants to use {other}"),
        };
        let preview = tools::preview(&call.name, &call.arguments, &ctx);
        // These work in the project folder. Without one, Allow could only
        // fail, so the card asks for the folder first.
        let folder = self
            .streaming_id
            .and_then(|id| self.store.index_of(id))
            .and_then(|ix| self.store.conversations[ix].workspace.clone());
        let in_folder = matches!(
            call.name.as_str(),
            "run_command" | "write_file" | "edit_file"
        );
        let answers = if in_folder && folder.is_none() {
            h_flex()
                .gap_2()
                .items_center()
                .child(
                    Button::new("approval-folder")
                        .primary()
                        .small()
                        .icon(Icon::new(AppIcon::Folder))
                        .label("Choose a project folder")
                        .on_click(
                            cx.listener(|this, _, window, cx| this.attach_workspace(window, cx)),
                        ),
                )
                .child(
                    Button::new("deny")
                        .plain(cx)
                        .small()
                        .label("Deny")
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.resolve_call(Verdict::Deny(String::new()), cx)
                        })),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child("No project folder is attached yet."),
                )
        } else {
            h_flex()
                .gap_2()
                .items_center()
                .child(
                    Button::new("allow")
                        .primary()
                        .small()
                        .label("Allow")
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.take_reason(window, cx);
                            this.resolve_call(Verdict::Allow { remember: false }, cx)
                        })),
                )
                // An upload, or a sign-in, is asked about each time, so there
                // is no allowing it for the session.
                .children(
                    (gate::grants_for_session(&call.name, &args)
                        && !tools::asked_each_time(&call.name))
                    .then(|| {
                        Button::new("allow-session")
                            .framed(cx)
                            .small()
                            .label("Allow for this session")
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.take_reason(window, cx);
                                this.resolve_call(Verdict::Allow { remember: true }, cx)
                            }))
                    }),
                )
                .children(self.offer.as_ref().map(|rule| {
                    Button::new("allow-here")
                        .framed(cx)
                        .small()
                        .label(format!("Always allow `{}` here", rule.pattern))
                        .on_click(
                            cx.listener(|this, _, _, cx| this.resolve_call(Verdict::AllowHere, cx)),
                        )
                }))
                // Beside Deny, which sends what is typed here; enter in the
                // field denies too.
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .child(Input::new(&self.deny_reason).small().w_full()),
                )
                .child(
                    Button::new("deny")
                        .plain(cx)
                        .small()
                        .label("Deny")
                        .on_click(cx.listener(|this, _, window, cx| this.deny_call(window, cx))),
                )
        };
        Some(
            card.child(
                v_flex()
                    .gap_0p5()
                    .child(
                        div()
                            .text_sm()
                            .font_family(HEADING_FONT)
                            .font_weight(FontWeight::MEDIUM)
                            .child(title),
                    )
                    // Where a command runs, which its text does not say.
                    .when_some(
                        folder.filter(|_| call.name == "run_command"),
                        |this, folder| {
                            this.child(
                                div()
                                    .text_xs()
                                    .truncate()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(format!("In {}", folder.display())),
                            )
                        },
                    )
                    // Why Auto mode's judge left it to the user.
                    .when_some(self.why.clone(), |this, why| {
                        this.child(
                            div()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child(why),
                        )
                    }),
            )
            .child(
                // Scrolls only once the preview is long, so a one-line command
                // does not sit in a tall empty panel.
                div()
                    .p_2()
                    .min_w_0()
                    .rounded(cx.theme().radius)
                    .bg(cx.theme().background)
                    .child(crate::ui::code_block(
                        "preview",
                        tools::clamp_lines(&preview, 400),
                        px(240.),
                        window,
                        cx,
                    )),
            )
            .child(answers)
            .into_any_element(),
        )
    }

    /// The card for the first helper's call waiting on the user in session
    /// `root`, or `None` when no helper there is waiting.
    pub(crate) fn helper_card(
        &self,
        root: u64,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let request = self
            .agent_asks
            .iter()
            .find(|request| request.root == root)?;
        let call = &request.call;
        let summary = tools::summary(&call.name, &call.arguments);
        let title = wants(&format!("Helper {}", request.label), &call.name, &summary);
        let answer = |decision: Decision| {
            let (label, id) = (request.label.clone(), call.id.clone());
            cx.listener(move |this, _, _, cx| {
                this.answer_agent(root, &label, &id, decision.clone(), cx)
            })
        };
        let (once, for_run, deny) = (
            answer(Decision::Once),
            answer(Decision::ForRun),
            answer(Decision::Deny(String::new())),
        );
        let card = permission(
            frame(cx),
            "helper-preview",
            title,
            &request.preview,
            window,
            cx,
        )
        // Why its gate could not decide it, such as the judge being unsure.
        .when(!request.why.is_empty(), |card| {
            card.child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(request.why.clone()),
            )
        });
        Some(
            card.child(
                h_flex()
                    .gap_2()
                    .child(
                        Button::new("helper-allow")
                            .primary()
                            .small()
                            .label("Allow")
                            .on_click(once),
                    )
                    .when_some(request.for_run(), |row, pattern| {
                        // Core remembers this for the rule the call falls
                        // under, for the helper and the helpers it starts,
                        // so they ask no more about it; a call no rule
                        // covers safely is offered no such answer.
                        let label = format!("Allow `{pattern}` for this run");
                        row.child(
                            Button::new("helper-allow-run")
                                .outline()
                                .small()
                                .label(label)
                                .on_click(for_run),
                        )
                    })
                    .child(
                        Button::new("helper-deny")
                            .ghost()
                            .small()
                            .label("Deny")
                            .on_click(deny),
                    ),
            )
            .into_any_element(),
        )
    }
}

/// The answers an `ask_user` call offers, in order.
fn options_in(args: &serde_json::Value) -> Vec<String> {
    args["options"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str())
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

/// The letter that picks the answer at `n`, for the first 26.
fn option_letter(n: usize) -> Option<char> {
    u8::try_from(n)
        .ok()
        .filter(|&n| n < 26)
        .map(|n| char::from(b'a' + n))
}

/// The box an approval card sits in.
fn frame(cx: &App) -> Div {
    v_flex()
        .w_full()
        .gap_3()
        .p_3()
        .rounded(cx.theme().radius_lg)
        .border_1()
        .border_color(cx.theme().primary.opacity(0.5))
        .bg(cx.theme().secondary.opacity(0.3))
}

/// What a card says `who` wants to do with a call of `name`.
fn wants(who: &str, name: &str, summary: &str) -> String {
    match name {
        "write_file" => format!("{who} wants to write {summary}"),
        "edit_file" => format!("{who} wants to edit {summary}"),
        "run_command" => format!("{who} wants to run a command"),
        "browse" | "use_page" => format!("{who} wants to {summary}"),
        "search_web" => format!("{who} wants to search the web"),
        other => format!("{who} wants to use {other}"),
    }
}

/// `card` with what a call wants and the tool's preview of it, ready for its
/// answers; `id` keeps the preview's scrolling apart from another card's.
fn permission(
    card: Div,
    id: &'static str,
    title: String,
    preview: &str,
    window: &mut Window,
    cx: &mut App,
) -> Div {
    card.child(
        div()
            .text_sm()
            .font_family(HEADING_FONT)
            .font_weight(FontWeight::MEDIUM)
            .child(title),
    )
    .child(
        // Scrolls only once the preview is long, so a one-line command
        // does not sit in a tall empty panel.
        div()
            .p_2()
            .min_w_0()
            .rounded(cx.theme().radius)
            .bg(cx.theme().background)
            .child(crate::ui::code_block(
                id,
                tools::clamp_lines(preview, 400),
                px(240.),
                window,
                cx,
            )),
    )
}

#[cfg(test)]
mod tests {
    // Not `use super::*`: the prelude re-exports gpui, whose `test` macro
    // would shadow the built-in attribute.
    use super::{option_letter, options_in};

    #[test]
    fn a_question_offers_its_answers_in_order_each_under_a_letter() {
        let args = serde_json::json!({"question": "Which?", "options": ["Red", 3, "Blue"]});
        assert_eq!(options_in(&args), ["Red", "Blue"]);
        assert_eq!(options_in(&serde_json::json!({})), Vec::<String>::new());
        assert_eq!(option_letter(0), Some('a'));
        assert_eq!(option_letter(25), Some('z'));
        assert_eq!(option_letter(26), None);
    }
}
