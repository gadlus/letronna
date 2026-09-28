//! The card the chat shows when a tool needs the user: approve a write or a
//! command, or answer an `ask_user` question. A helper's call gets the same
//! card under the helper's name, with the answers a helper can be given.

use crate::prelude::*;
use letronna_core::approvals::Decision;
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

        let args = tools::parse_args(&call.arguments).unwrap_or_default();
        let card = frame(cx);

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
                        .outline()
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

        let title = wants(BRAND, &call.name, &summary);
        let preview = tools::preview(&call.name, &call.arguments, &ctx);
        let card = permission(card, "preview", title, &preview, window, cx);
        Some(
            card.child(
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
                            .outline()
                            .small()
                            .label("Allow for this session")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.resolve_call(Verdict::Allow { remember: true }, cx)
                            })),
                    )
                    .child(Button::new("deny").ghost().small().label("Deny").on_click(
                        cx.listener(|this, _, _, cx| this.resolve_call(Verdict::Deny, cx)),
                    )),
            )
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
            cx.listener(move |this, _, _, cx| this.answer_agent(root, &label, &id, decision, cx))
        };
        let (once, for_run, deny) = (
            answer(Decision::Once),
            answer(Decision::ForRun),
            answer(Decision::Deny),
        );
        let card = permission(
            frame(cx),
            "helper-preview",
            title,
            &request.preview,
            window,
            cx,
        );
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
                    .child(
                        // Core remembers this for the tool, for the helper
                        // and the helpers it starts, so they ask no more.
                        Button::new("helper-allow-run")
                            .outline()
                            .small()
                            .label("Allow for this run")
                            .on_click(for_run),
                    )
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
