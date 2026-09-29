//! The card the chat shows when a tool needs the user: approve a write or a
//! command, answer an `ask_user` question, or approve a plan.

use crate::prelude::*;
use letronna_core::tools;

impl Chat {
    pub(crate) fn submit_answer(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let text = self.answer.read(cx).value().trim().to_owned();
        if text.is_empty() {
            return;
        }
        self.answer
            .update(cx, |input, cx| input.set_value("", window, cx));
        self.resolve_call(Verdict::Answer(text), cx);
    }

    /// The row under the messages while tools run: a progress line, or the
    /// card waiting on the user. `None` when nothing is queued.
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
            return Some(
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
                    .child(div().font_family(MONO_FONT).child(summary))
                    .into_any_element(),
            );
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
            let options: Vec<String> = args["options"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(|v| v.as_str())
                        .map(str::to_owned)
                        .collect()
                })
                .unwrap_or_default();
            let mut chips = h_flex().flex_wrap().gap_2();
            for (i, option) in options.into_iter().enumerate() {
                let pick = option.clone();
                chips = chips.child(
                    Button::new(("option", i))
                        .framed(cx)
                        .small()
                        .label(option)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.resolve_call(Verdict::Answer(pick.clone()), cx)
                        })),
                );
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
                                    this.resolve_call(Verdict::Deny, cx)
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
                        .on_click(
                            cx.listener(|this, _, _, cx| this.resolve_call(Verdict::Deny, cx)),
                        ),
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
                .child(
                    Button::new("allow")
                        .primary()
                        .small()
                        .label("Allow")
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.resolve_call(Verdict::Allow { remember: false }, cx)
                        })),
                )
                .child(
                    Button::new("allow-session")
                        .framed(cx)
                        .small()
                        .label("Allow for this session")
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.resolve_call(Verdict::Allow { remember: true }, cx)
                        })),
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
                .child(
                    Button::new("deny")
                        .plain(cx)
                        .small()
                        .label("Deny")
                        .on_click(
                            cx.listener(|this, _, _, cx| this.resolve_call(Verdict::Deny, cx)),
                        ),
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
                    ),
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
}
