use crate::prelude::*;
use gpui_component::notification::Notification;
use letronna_core::failure::{self, Failure, FailureKind, Fix, Kept};
use letronna_core::tool_runs::Run;

actions!(messages, [ToggleRun]);

/// A failed turn set to be asked again when its provider's wait is over.
pub(crate) struct WaitingRetry {
    pub(crate) session: u64,
    /// When, in seconds since 1970.
    pub(crate) at: u64,
}

impl Chat {
    /// An id for part of a message that also names its session, so state
    /// kept by id -- a reply's parsed markdown, its fade-in -- never shows
    /// in another session with a message at the same place.
    pub(crate) fn row_id(&self, name: &str, index: usize) -> ElementId {
        let session = self.store.conversations[self.store.active].id;
        (SharedString::from(format!("{name}-{session}")), index).into()
    }

    /// A row id for a text view that also names the theme it is drawn in:
    /// the view keeps the syntax colours of its first draw, so a new theme
    /// takes a new view.
    fn text_id(&self, name: &str, index: usize) -> ElementId {
        self.row_id(&format!("{name}-{}", crate::theme::generation()), index)
    }

    /// Whether a thought or a tool's output is shown open, by session and
    /// place, so opening one does not open its twin in another session.
    fn open_key(&self, index: usize) -> (u64, usize) {
        (self.store.conversations[self.store.active].id, index)
    }

    /// One message. `bar` is its turn's action buttons, when this message
    /// is the one that ends the turn.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn message_row(
        &self,
        index: usize,
        message: &Message,
        labeled: bool,
        streaming: bool,
        bar: Option<AnyElement>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        if message.role == "tool" {
            // A turn stopped part way ends on a tool; its buttons go under it.
            return v_flex()
                .w_full()
                .gap_1()
                .child(self.tool_row(index, message, window, cx))
                .children(bar)
                .into_any_element();
        }
        if message.role == failure::ROLE {
            return self.failure_row(index, message, labeled, bar, cx);
        }
        if message.notice {
            return self.notice_row(index, message, window, cx);
        }
        let is_user = message.role == "user";
        let session = self.store.conversations[self.store.active].id;
        let editing = is_user && self.editing_message == Some((session, index));
        // Where the turn's buttons go: at the end of your prompt's label, in
        // a reply's copy-and-usage line, or under a reply that has neither.
        // In the two lines they take no room of their own.
        let usage_row =
            !is_user && !message.content.is_empty() && (!streaming || message.tokens > 0);
        let (on_label, in_usage, under) = match (is_user, usage_row) {
            (true, _) => (bar.filter(|_| !editing), None, None),
            (false, true) => (None, bar, None),
            (false, false) => (None, None, bar),
        };
        // A reply that only asked for tools: its tool rows say what happened.
        let tool_only = !is_user && message.content.is_empty() && !message.tool_calls.is_empty();
        let label: String = if message.model.is_empty() {
            self.store.model(self.store.active).1.to_owned()
        } else {
            message.model.clone()
        };
        let leading = px(24.);
        let cx_muted = cx.theme().muted_foreground;
        let md = self.text_id("md", index);
        // Not while a dialog is up: the text view starts a selection on any
        // press inside it, even one on a dialog drawn over it, and that press
        // never ends -- confirming a rewind left the reply half selected.
        let selectable = !window.has_active_dialog(cx);
        let body: AnyElement = if editing {
            // Sending takes back what followed, unless more was asked: that
            // stays, and the edit goes to a fork, which Send says first.
            let forks = self.asked_after(index);
            v_flex()
                .gap_2()
                .child(Input::new(&self.edit_message).appearance(false))
                .child(
                    h_flex()
                        .gap_2()
                        .items_center()
                        .justify_end()
                        .when(forks, |row| {
                            row.child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .text_xs()
                                    .text_color(cx_muted)
                                    .child("What you asked after this stays here."),
                            )
                        })
                        .child(
                            Button::new("edit-cancel")
                                .plain(cx)
                                .small()
                                .label("Cancel")
                                .on_click(
                                    cx.listener(|this, _, window, cx| this.cancel_edit(window, cx)),
                                ),
                        )
                        .child(
                            Button::new("edit-send")
                                .primary()
                                .small()
                                .label(if forks { "Send in a fork" } else { "Send" })
                                .on_click(
                                    cx.listener(|this, _, window, cx| this.send_edit(window, cx)),
                                ),
                        ),
                )
                .into_any_element()
        } else if is_user {
            // `/init`, a saved workflow's command and `@` references show
            // as typed, not as what they sent; a chat app's message can quote
            // someone else's, fenced.
            let content = letronna_core::references::typed(&message.content);
            let typed = letronna_core::init::typed(content).map(str::to_owned);
            let said = match typed.or_else(|| letronna_core::workflow::saved::typed(content)) {
                Some(typed) => typed,
                None => letronna_core::untrusted::shown("", content).0,
            };
            TextView::markdown(md, said, window, cx)
                .selectable(selectable)
                .text_size(px(15.))
                .font_weight(FontWeight::MEDIUM)
                .line_height(leading)
                .into_any_element()
        } else if message.content.is_empty() && !streaming {
            // Stored with nothing said, by a run elsewhere -- the window keeps
            // no such reply of its own -- and long done thinking.
            div().into_any_element()
        } else if message.content.is_empty() {
            h_flex()
                .gap_2()
                .items_center()
                .text_color(cx.theme().muted_foreground)
                .child(
                    crate::ui::busy::Spinner::new()
                        .xsmall()
                        .color(cx.theme().primary),
                )
                .child({
                    let live = self.turn.as_ref().filter(|_| streaming).map(|t| &t.live);
                    match (
                        live.and_then(|l| l.retrying),
                        live.is_some_and(|l| l.compacting),
                    ) {
                        (Some(retry), _) => retry.to_string(),
                        (None, true) => "Compacting…".to_owned(),
                        (None, false) => "Thinking…".to_owned(),
                    }
                })
                .into_any_element()
        } else {
            TextView::markdown(md, message.content.clone(), window, cx)
                .selectable(selectable)
                .line_height(leading)
                .style(gpui_component::text::TextViewStyle::default().heading_gap(gpui::rems(1.)))
                .code_block_actions(move |block, _, _| {
                    let code = block.code();
                    let mut hasher = std::hash::DefaultHasher::new();
                    std::hash::Hash::hash(&code, &mut hasher);
                    let id = std::hash::Hasher::finish(&hasher);
                    let lang = block.lang().unwrap_or_default();
                    h_flex()
                        .gap_2()
                        .items_center()
                        .when(!lang.is_empty(), |this| {
                            this.child(div().text_xs().text_color(cx_muted).child(lang.to_string()))
                        })
                        .child(
                            gpui_component::clipboard::Clipboard::new(SharedString::from(format!(
                                "copy-{index}-{id}"
                            )))
                            .value(code),
                        )
                })
                .into_any_element()
        };
        v_flex()
            .w_full()
            // Closer to its own text than the turn's rows are to each other,
            // so a label reads as the head of what follows it.
            .gap_1p5()
            // Your prompt is a plain row under its own label, its buttons at
            // the label's end; the line keeps their height so nothing moves
            // when they show.
            .when(is_user, |this| {
                this.child(
                    h_flex()
                        .h_5()
                        .items_center()
                        .justify_between()
                        .gap_2()
                        .child(
                            div()
                                .text_xs()
                                .font_family(HEADING_FONT)
                                .font_weight(FontWeight::MEDIUM)
                                .text_color(cx.theme().link)
                                .child("You"),
                        )
                        .children(on_label),
                )
            })
            .when(labeled && !is_user, |this| {
                this.child(
                    h_flex()
                        .items_center()
                        .gap_2()
                        .text_xs()
                        .font_family(HEADING_FONT)
                        .child(
                            div()
                                .font_weight(FontWeight::MEDIUM)
                                .text_color(cx.theme().link)
                                .child(label),
                        )
                        .when(streaming, |this| {
                            this.child(
                                div()
                                    .size(px(6.))
                                    .rounded_full()
                                    .bg(cx.theme().primary)
                                    .opacity(crate::ui::busy::breathing(window, cx)),
                            )
                        }),
                )
            })
            // Thinking is shown when it was asked for, the same switch the
            // terminal reads. Without this the window showed it whatever the
            // setting said, and the two halves disagreed about the same file.
            .when(
                !is_user && !message.reasoning.is_empty() && self.store.settings.thinking,
                |this| this.child(self.thinking_block(index, message, streaming, window, cx)),
            )
            // The markdown renderer reports an intrinsic width wider than the
            // column for some documents (lists after a wrapped paragraph),
            // which pushes the whole chat past the composer. Clipping here
            // clamps it; min_w_0 would instead collapse list layout.
            .when(!tool_only, |this| {
                this.child(div().w_full().overflow_hidden().pb_1().child(body))
            })
            .when(is_user && !message.images.is_empty(), |this| {
                this.child(crate::ui::pictures::sent_row(&message.images, cx))
            })
            // A reply is often wanted whole, and selecting it across
            // paragraphs and code blocks is fiddly.
            .when(usage_row, |this| {
                this.child(
                    h_flex()
                        .gap_2()
                        .items_center()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .when(!streaming, |row| {
                            row.child(
                                gpui_component::clipboard::Clipboard::new(("copy-reply", index))
                                    .value(message.content.clone()),
                            )
                        })
                        .when(message.tokens > 0, |row| row.child(usage_line(message)))
                        .when(message.interrupted, |row| row.child("(stopped)"))
                        .children(in_usage),
                )
            })
            .children(under)
            .with_animation(
                self.row_id("appear", index),
                Animation::new(std::time::Duration::from_millis(260))
                    .with_easing(gpui::ease_out_quint()),
                |el, delta| el.opacity(delta).mt(px(10. * (1. - delta))),
            )
            .into_any_element()
    }

    /// A message sent while the reply runs, waiting for the reply's next
    /// step: your card, faded, with a way to take it back first.
    pub(crate) fn queued_row(&self, n: usize, text: &str, cx: &mut Context<Self>) -> AnyElement {
        let key = if cfg!(target_os = "macos") {
            "⌘Enter"
        } else {
            "Ctrl+Enter"
        };
        // Sending now never stops a running tool, so then it waits for it.
        let now = if self.turn.as_ref().is_some_and(cuts_in) {
            format!("Send now, stopping the reply where it is ({key})")
        } else {
            format!("Send as soon as the tool is done ({key})")
        };
        v_flex()
            .w_full()
            .gap_1()
            .child(
                h_flex()
                    .justify_between()
                    .items_center()
                    .text_xs()
                    .font_family(BODY_FONT)
                    .text_color(cx.theme().muted_foreground)
                    .child("Queued · goes in after this step")
                    .child(
                        h_flex()
                            .gap_0p5()
                            .child(
                                Button::new(("queued-now", n))
                                    .ghost()
                                    .xsmall()
                                    .icon(Icon::new(IconName::ArrowUp))
                                    .tooltip(now)
                                    .on_click(cx.listener(|this, _, _, cx| this.send_now(cx))),
                            )
                            .child(
                                Button::new(("queued-back", n))
                                    .ghost()
                                    .xsmall()
                                    .icon(Icon::new(AppIcon::Pencil))
                                    .tooltip("Back to the box")
                                    .on_click(cx.listener(move |this, _, window, cx| {
                                        this.unqueue(n, true, window, cx)
                                    })),
                            )
                            .child(
                                Button::new(("queued-drop", n))
                                    .ghost()
                                    .xsmall()
                                    .icon(Icon::new(IconName::Close))
                                    .tooltip("Don't send")
                                    .on_click(cx.listener(move |this, _, window, cx| {
                                        this.unqueue(n, false, window, cx)
                                    })),
                            ),
                    ),
            )
            .child(
                div()
                    .w_full()
                    .px_4()
                    .py_3()
                    .rounded(cx.theme().radius_lg)
                    .border_1()
                    .border_color(cx.theme().border)
                    .text_color(cx.theme().muted_foreground)
                    .child(text.to_owned()),
            )
            .into_any_element()
    }

    /// The side question on show for `session`, under the conversation and
    /// in no turn of it, since the session does not keep it.
    pub(crate) fn aside_row(
        &self,
        session: u64,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let aside = self
            .aside
            .as_ref()
            .filter(|aside| aside.session == session)?;
        let muted = cx.theme().muted_foreground;
        // Rendered and selectable as a reply is, since it is the only copy.
        let answer = match &aside.answer {
            None => div()
                .text_color(muted)
                .child("Thinking…")
                .into_any_element(),
            Some(Ok(reply)) => v_flex()
                .gap_1()
                .text_color(muted)
                .child(
                    TextView::markdown(self.row_id("aside", 0), reply.content.clone(), window, cx)
                        .selectable(!window.has_active_dialog(cx))
                        .line_height(px(26.)),
                )
                .child(div().text_xs().child(usage_line(reply)))
                .into_any_element(),
            Some(Err(error)) => div()
                .text_color(cx.theme().danger)
                .child(error.clone())
                .into_any_element(),
        };
        let row = v_flex()
            .w_full()
            .gap_1()
            .child(
                h_flex()
                    .justify_between()
                    .items_center()
                    .text_xs()
                    .font_family(HEADING_FONT)
                    .text_color(cx.theme().muted_foreground)
                    .child("Aside · not kept in the session")
                    .child(
                        Button::new("aside-close")
                            .ghost()
                            .xsmall()
                            .icon(Icon::new(IconName::Close))
                            .tooltip_with_action("Close", &Dismiss, None)
                            .on_click(cx.listener(|this, _, _, cx| {
                                if let Some(aside) = this.aside.take() {
                                    aside.cancel.set();
                                }
                                cx.notify();
                            })),
                    ),
            )
            .child(
                v_flex()
                    .w_full()
                    .gap_2()
                    .px_4()
                    .py_3()
                    .rounded(cx.theme().radius_lg)
                    .border_1()
                    .border_color(cx.theme().border)
                    .child(aside.question.clone())
                    .child(answer),
            );
        Some(row.into_any_element())
    }

    /// Keep the turn that just failed in its session, to be drawn where it
    /// failed; false when there is nothing to keep, as for a stopped turn.
    pub(crate) fn keep_failure(&mut self, failure: &Failure) -> bool {
        let Some(ix) = self.streaming_id.and_then(|id| self.store.index_of(id)) else {
            return false;
        };
        let Some(model) = self.turn.as_ref().map(|turn| turn.connection.model.clone()) else {
            return false;
        };
        let hosted = self.store.model(ix).0 == LETRONNA;
        let Some(message) = failure.kept(&model, hosted, backend::now()) else {
            return false;
        };
        let session = self.store.conversations[ix].id;
        // A wait set for an earlier failure there is for a turn now gone by.
        if self
            .waiting_retry
            .as_ref()
            .is_some_and(|w| w.session == session)
        {
            self.waiting_retry = None;
        }
        self.store.conversations[ix].messages.push(message);
        if ix != self.store.active {
            self.unseen.insert(session);
        }
        self.save();
        true
    }

    /// Whether the session `session` still ends on a failed turn, the one
    /// thing a retry there carries on from.
    pub(crate) fn ends_failed(&self, session: u64) -> bool {
        self.store.index_of(session).is_some_and(|ix| {
            self.store.conversations[ix]
                .messages
                .last()
                .is_some_and(|m| m.role == failure::ROLE)
        })
    }

    /// Ask the failed turn in `session` again at `at`, counting down on its
    /// row until then. The count looks once a second and stops as soon as
    /// the wait is cancelled or replaced, so nothing ticks once it is over.
    pub(crate) fn wait_to_retry(&mut self, session: u64, at: u64, cx: &mut Context<Self>) {
        let same = move |w: &WaitingRetry| w.session == session && w.at == at;
        if self.waiting_retry.as_ref().is_some_and(same) {
            return;
        }
        self.waiting_retry = Some(WaitingRetry { session, at });
        cx.notify();
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(std::time::Duration::from_secs(1))
                    .await;
                let waiting = this.update(cx, |this, cx| {
                    // A turn since, or the session gone, leaves nothing to
                    // wait for, and the count must not tick on until `at`.
                    if this.waiting_retry.as_ref().is_some_and(same) && !this.ends_failed(session) {
                        this.waiting_retry = None;
                    }
                    let waiting = this.waiting_retry.as_ref().is_some_and(same);
                    // In the same update as the check, so nothing can take
                    // the agent in between.
                    if waiting && retry_due(backend::now(), at, this.busy) {
                        this.retry_failed(session, cx);
                        return false;
                    }
                    cx.notify();
                    waiting
                });
                if !matches!(waiting, Ok(true)) {
                    break;
                }
            }
        })
        .detach();
    }

    /// A turn that failed: why, in a few words, one line of what the provider
    /// said, and on the newest, at most two ways out, the likely one first.
    fn failure_row(
        &self,
        index: usize,
        message: &Message,
        labeled: bool,
        bar: Option<AnyElement>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let conversation = &self.store.conversations[self.store.active];
        let session = conversation.id;
        let kept = message.failed.as_deref().copied().unwrap_or(Kept {
            kind: FailureKind::Unknown,
            until: None,
            hosted: false,
        });
        let now = backend::now();
        // One further up is history: something came after it.
        let newest = index + 1 == conversation.messages.len();
        let running_here = self.busy && self.streaming_id == Some(session);
        let muted = cx.theme().muted_foreground;
        let danger = cx.theme().danger;
        let waiting = self
            .waiting_retry
            .as_ref()
            .filter(|w| w.session == session && Some(w.at) == kept.until);
        let actions =
            if !newest || running_here {
                None
            } else if let Some(waiting) = waiting {
                let when = if now >= waiting.at {
                    "Retrying once the reply running now is done".to_owned()
                } else {
                    let left = countdown(waiting.at - now);
                    format!("Retrying at {}, in {left}", failure::clock(waiting.at))
                };
                Some(
                    h_flex()
                        .gap_2()
                        .items_center()
                        .child(div().text_xs().text_color(muted).child(when))
                        .child(
                            Button::new(self.row_id("failure-cancel", index))
                                .plain(cx)
                                .small()
                                .label("Cancel")
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.waiting_retry = None;
                                    cx.notify();
                                })),
                        ),
                )
            } else {
                let fixes = kept.fixes(now).into_iter().enumerate();
                Some(h_flex().gap_2().items_center().children(
                    fixes.map(|(n, fix)| self.fix_button(index, n == 0, fix, message, cx)),
                ))
            };
        v_flex()
            .w_full()
            .gap_1p5()
            // Heading its turn as a reply would, so one asked again after it
            // reads as the same turn going on.
            .when(labeled, |this| {
                this.child(
                    div()
                        .text_xs()
                        .font_family(HEADING_FONT)
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(cx.theme().link)
                        .child(message.model.clone()),
                )
            })
            .child(
                v_flex()
                    .w_full()
                    .min_w_0()
                    .gap_1()
                    .pl_3()
                    .border_l_2()
                    .border_color(danger)
                    .child(
                        h_flex()
                            .gap_1p5()
                            .items_center()
                            .child(
                                Icon::new(IconName::TriangleAlert)
                                    .size_3p5()
                                    .text_color(danger),
                            )
                            .child(
                                div()
                                    .text_sm()
                                    .font_weight(FontWeight::MEDIUM)
                                    .child(kept.cause(failure::clock)),
                            ),
                    )
                    .child(
                        div()
                            .min_w_0()
                            .text_xs()
                            .text_color(muted)
                            // Clipped, not truncated: gpui keeps the first
                            // width a truncated line was measured at, here
                            // too narrow, and drew the line as "…".
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .child(letronna_core::tools::one_line(&message.content)),
                    )
                    .children(actions.map(|actions| div().pt_1().child(actions))),
            )
            .children(bar)
            .into_any_element()
    }

    /// The button for one way out of the failure kept in `message`; the
    /// `first` is the likely one, and framed to say so.
    fn fix_button(
        &self,
        index: usize,
        first: bool,
        fix: Fix,
        message: &Message,
        cx: &mut Context<Self>,
    ) -> Button {
        let session = self.store.conversations[self.store.active].id;
        let (label, icon) = match fix {
            Fix::Retry => ("Retry".to_owned(), Icon::new(AppIcon::Reload)),
            Fix::RetryAt(at) => (
                format!("Retry at {}", failure::clock(at)),
                Icon::new(AppIcon::Clock),
            ),
            Fix::AddCredit => ("Add credit".to_owned(), Icon::new(AppIcon::PopOut)),
            Fix::ChooseModel => ("Choose a model".to_owned(), Icon::new(AppIcon::Sparkles)),
            Fix::SignIn => ("Sign in".to_owned(), Icon::new(AppIcon::User)),
            Fix::CopyDetails => ("Copy details".to_owned(), Icon::new(IconName::Copy)),
        };
        let button = Button::new(self.row_id(if first { "fix" } else { "fix-other" }, index))
            .small()
            .icon(icon)
            .label(label)
            // Another session's reply owns the agent until it is done.
            .disabled(self.busy && matches!(fix, Fix::Retry | Fix::RetryAt(_)));
        let button = if first {
            button.framed(cx)
        } else {
            button.plain(cx)
        };
        match fix {
            Fix::Retry => {
                button.on_click(cx.listener(move |this, _, _, cx| this.retry_failed(session, cx)))
            }
            Fix::RetryAt(at) => button
                .on_click(cx.listener(move |this, _, _, cx| this.wait_to_retry(session, at, cx))),
            Fix::AddCredit => {
                button.on_click(|_, _, cx| cx.open_url(&letronna_core::auth::checkout()))
            }
            Fix::ChooseModel => button.on_click(cx.listener(|this, _, window, cx| {
                this.model_open = true;
                this.model_search.update(cx, |search, cx| {
                    search.set_value("", window, cx);
                    search.focus(window, cx);
                });
                cx.notify();
            })),
            Fix::SignIn => button.on_click(cx.listener(|this, _, window, cx| {
                this.open_account(window, cx);
                this.sign_in(cx);
            })),
            Fix::CopyDetails => {
                let details = failure::details(message);
                button.on_click(move |_, window, cx| {
                    cx.write_to_clipboard(ClipboardItem::new_string(details.clone()));
                    window.push_notification(Notification::success("Copied."), cx);
                })
            }
        }
    }

    /// Something that happened meanwhile, such as a job exiting: one quiet
    /// line rather than a card, since nobody here said it; click to read it.
    fn notice_row(
        &self,
        index: usize,
        message: &Message,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let key = self.open_key(index);
        let open = self.thinking_open.contains(&key);
        // The first line tells the model what a notice is not; here the row's
        // look says so, and it starts at what happened.
        let body = message
            .content
            .split_once('\n')
            .map_or("", |(_, rest)| rest)
            .to_owned();
        let first = body.lines().next().unwrap_or_default();
        let headline = letronna_core::tools::one_line(first.strip_prefix("- ").unwrap_or(first));
        v_flex()
            .w_full()
            .gap_1()
            .child(
                h_flex()
                    .id(("notice", index))
                    .h(px(26.))
                    .min_w_0()
                    .w_full()
                    .px_2()
                    .gap_1p5()
                    .items_center()
                    .rounded(cx.theme().radius)
                    .cursor_pointer()
                    .text_xs()
                    .font_family(HEADING_FONT)
                    .text_color(cx.theme().muted_foreground)
                    .hover(|s| s.bg(cx.theme().secondary.opacity(0.5)))
                    .child(
                        Icon::new(if open {
                            IconName::ChevronDown
                        } else {
                            IconName::ChevronRight
                        })
                        .size_3(),
                    )
                    .child("Notice")
                    .child(div().min_w_0().truncate().child(headline))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if !this.thinking_open.remove(&key) {
                            this.thinking_open.insert(key);
                        }
                        cx.notify();
                    })),
            )
            .when(open, |this| {
                this.child(
                    div()
                        .ml_2()
                        .pl_3()
                        .min_w_0()
                        .border_l_2()
                        .border_color(cx.theme().border)
                        .text_color(cx.theme().muted_foreground)
                        .child(crate::ui::code_block(
                            ("notice-out", index),
                            body,
                            px(220.),
                            window,
                            cx,
                        )),
                )
            })
            .into_any_element()
    }

    /// A tool result: one quiet line, click to see the output.
    fn tool_row(
        &self,
        index: usize,
        message: &Message,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let key = self.open_key(index);
        // A failure is the output you need to read, so it starts open, in
        // full. One you declined yourself is no news, so it stays shut.
        let failed = message.is_error && !letronna_core::tools::is_declined(&message.content);
        let open = self.thinking_open.contains(&key) != failed;
        let summary = letronna_core::tools::summary(&message.name, &message.args);
        // The fences' tags are for the model; here a few words say where the
        // text came from.
        let (content, from) = letronna_core::untrusted::shown(&message.name, &message.content);
        let lines = content.lines().count();
        let one_line = letronna_core::tools::one_line;
        // A file a media tool made, while it is still where it was saved.
        let made = (!message.is_error)
            .then(|| letronna_core::media::saved_by(&message.name, &message.content))
            .flatten()
            .filter(|path| path.exists());
        let (status, color) = if failed {
            ("failed".to_owned(), cx.theme().danger)
        } else if message.is_error {
            ("declined".to_owned(), cx.theme().muted_foreground)
        } else if message.name == letronna_core::tools::ASK_USER {
            (
                format!("→ {}", one_line(&message.content)),
                cx.theme().foreground,
            )
        } else if let Some(name) = made.as_ref().and_then(|path| path.file_name()) {
            (
                name.to_string_lossy().into_owned(),
                cx.theme().muted_foreground,
            )
        } else {
            let count = if lines == 1 {
                "1 line".to_owned()
            } else {
                format!("{lines} lines")
            };
            (count, cx.theme().muted_foreground)
        };
        v_flex()
            .w_full()
            .gap_1()
            .child(
                h_flex()
                    .id(("tool", index))
                    .h(px(26.))
                    // Without this the row's nowrap text sets a min width that
                    // widens the whole column past the composer. It belongs
                    // here, not on the turn: clipping a turn breaks the
                    // markdown renderer's list layout.
                    .min_w_0()
                    .w_full()
                    .px_2()
                    .gap_1p5()
                    .items_center()
                    .rounded(cx.theme().radius)
                    .cursor_pointer()
                    .text_xs()
                    .font_family(HEADING_FONT)
                    .text_color(cx.theme().muted_foreground)
                    .active(|s| s.bg(cx.theme().secondary.opacity(0.5)))
                    .child(
                        Icon::new(if open {
                            IconName::ChevronDown
                        } else {
                            IconName::ChevronRight
                        })
                        .size_3(),
                    )
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
                            .child(summary),
                    )
                    .child(div().min_w_0().text_color(color).truncate().child(status))
                    .when_some(from, |row, from| {
                        row.child(
                            div()
                                .flex_none()
                                .text_color(cx.theme().muted_foreground)
                                .child(from),
                        )
                    })
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if !this.thinking_open.remove(&key) {
                            this.thinking_open.insert(key);
                        }
                        cx.notify();
                    })),
            )
            // What let the call run in Auto mode, lined up with its name.
            .when_some(self.row_tag(index), |this, tag| {
                this.child(
                    div()
                        .pl(px(26.))
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(tag),
                )
            })
            .when_some(made, |this, path| {
                let playing = self.video.as_ref().is_some_and(|v| v.path == path);
                let frame = self.video_frame.clone().filter(|_| playing);
                this.child(made_file(index, path, playing, frame, cx))
            })
            .when(open, |this| {
                this.child(
                    div()
                        .ml_2()
                        .pl_3()
                        .min_w_0()
                        .border_l_2()
                        .border_color(cx.theme().border)
                        .text_color(if failed {
                            cx.theme().danger
                        } else {
                            cx.theme().muted_foreground
                        })
                        .child(crate::ui::code_block(
                            ("tool-out", index),
                            letronna_core::tools::clamp_lines(&content, 400),
                            px(220.),
                            window,
                            cx,
                        )),
                )
            })
    }

    /// A run of tool calls as one line, which Enter or a click opens into
    /// its rows and folds again. While the run is `going`, the line shows
    /// the call running now rather than what the run has done.
    pub(crate) fn run_row(
        &self,
        run: Run,
        going: bool,
        tools: &[std::sync::Arc<dyn letronna_core::tools::Tool>],
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let conversation = &self.store.conversations[self.store.active];
        let key = (conversation.id, run.first);
        let open = self.runs_open.contains(&key);
        let focus = window
            .use_keyed_state(self.row_id("run-focus", run.first), cx, |_, cx| {
                cx.focus_handle().tab_stop(true)
            })
            .read(cx)
            .clone();
        // Only a call that runs without asking: one waiting on the user has
        // its card below, which says so.
        let running = self.turn.as_ref().filter(|_| going).and_then(|turn| {
            match turn.next(|call| self.decide(call, &self.rules)) {
                letronna_core::agent::Next::Run(call) => Some(call),
                _ => None,
            }
        });
        let theme = cx.theme();
        let (muted, plain, danger) = (theme.muted_foreground, theme.foreground, theme.danger);
        let line: Vec<AnyElement> = match running {
            Some(call) => vec![
                crate::ui::busy::Spinner::new()
                    .xsmall()
                    .color(theme.primary)
                    .into_any_element(),
                div()
                    .flex_none()
                    .text_color(plain)
                    .child(call.name.clone())
                    .into_any_element(),
                div()
                    .font_family(MONO_FONT)
                    .min_w_0()
                    .truncate()
                    .child(letronna_core::tools::summary(&call.name, &call.arguments))
                    .into_any_element(),
            ],
            None => {
                let told = letronna_core::tool_runs::tell(&conversation.messages, run, tools);
                let coloured = [(told.did, plain), (told.failed, danger), (told.took, muted)];
                let mut parts = Vec::new();
                for (text, color) in coloured {
                    if text.is_empty() {
                        continue;
                    }
                    if !parts.is_empty() {
                        parts.push(div().flex_none().child("·").into_any_element());
                    }
                    parts.push(
                        div()
                            .min_w_0()
                            .truncate()
                            .text_color(color)
                            .child(text)
                            .into_any_element(),
                    );
                }
                parts
            }
        };
        let toggle = move |this: &mut Self, cx: &mut Context<Self>| {
            if !this.runs_open.remove(&key) {
                this.runs_open.insert(key);
            }
            cx.notify();
        };
        h_flex()
            .id(self.row_id("run", run.first))
            .track_focus(&focus)
            .key_context("ToolRun")
            .h(px(26.))
            // As a tool row's: its nowrap text must not widen the column.
            .min_w_0()
            .w_full()
            .px_2()
            .gap_1p5()
            .items_center()
            .rounded(theme.radius)
            .cursor_pointer()
            .text_xs()
            .font_family(HEADING_FONT)
            .text_color(muted)
            .focus(|s| s.bg(theme.secondary))
            .active(|s| s.bg(theme.secondary.opacity(0.5)))
            .child(
                Icon::new(if open {
                    IconName::ChevronDown
                } else {
                    IconName::ChevronRight
                })
                .size_3(),
            )
            .children(line)
            // As on a tool row, a press leaves the keys in the composer;
            // Tab is what picks the line out.
            .on_any_mouse_down(|_, window, _| window.prevent_default())
            // gpui also clicks a focused element when Enter comes back up,
            // which after ToggleRun folded the line straight back again.
            .on_click(cx.listener(move |this, event: &ClickEvent, _, cx| {
                if !event.is_keyboard() {
                    toggle(this, cx);
                }
            }))
            .on_action(cx.listener(move |this, _: &ToggleRun, _, cx| toggle(this, cx)))
            .into_any_element()
    }

    fn thinking_block(
        &self,
        index: usize,
        message: &Message,
        streaming: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let key = self.open_key(index);
        let open = streaming || self.thinking_open.contains(&key);
        let reasoning = message.reasoning.clone();
        v_flex()
            .w_full()
            .gap_1()
            .child(
                h_flex()
                    .id(("think", index))
                    .h(px(26.))
                    .px_2()
                    .gap_1p5()
                    .items_center()
                    .rounded(cx.theme().radius)
                    .cursor_pointer()
                    .text_xs()
                    .font_family(HEADING_FONT)
                    .text_color(cx.theme().muted_foreground)
                    .active(|s| s.bg(cx.theme().secondary.opacity(0.5)))
                    .child(
                        Icon::new(if open {
                            IconName::ChevronDown
                        } else {
                            IconName::ChevronRight
                        })
                        .size_3(),
                    )
                    .child(if streaming {
                        "Thinking…"
                    } else {
                        "Thought process"
                    })
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if !this.thinking_open.remove(&key) {
                            this.thinking_open.insert(key);
                        }
                        cx.notify();
                    })),
            )
            .when(open, |this| {
                this.child(
                    div()
                        .ml_2()
                        .pl_3()
                        .border_l_2()
                        .border_color(cx.theme().border)
                        .text_sm()
                        .text_color(cx.theme().muted_foreground)
                        .child(
                            TextView::markdown(
                                self.text_id("reason", index),
                                reasoning,
                                window,
                                cx,
                            )
                            .selectable(!window.has_active_dialog(cx))
                            .line_height(px(21.)),
                        ),
                )
            })
    }

    pub(crate) fn empty_state(
        &self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        // Without a project folder the tools have nowhere to read or write,
        // and the only other place that says so is a chip in the status bar.
        let no_project = self.store.conversations[self.store.active]
            .workspace
            .is_none();
        v_flex()
            .relative()
            .flex_1()
            .items_center()
            .justify_center()
            .gap_2()
            .child(
                div()
                    .font_family(WORDMARK_FONT)
                    .italic()
                    .text_size(px(72.))
                    .line_height(px(80.))
                    .child(BRAND),
            )
            .child(
                div()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child(self.greeting),
            )
            .when(no_project, |this| {
                this.child(
                    Button::new("choose-project")
                        .framed(cx)
                        .small()
                        .mt_4()
                        .icon(Icon::new(AppIcon::Folder))
                        .label("Choose a project folder")
                        .on_click(
                            cx.listener(|this, _, window, cx| this.attach_workspace(window, cx)),
                        ),
                )
            })
            .child(
                div()
                    .mt_4()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground.opacity(0.7))
                    .child("Enter to send  ·  Shift+Enter for a new line  ·  Esc to stop"),
            )
            .children(self.grain(&self.grains.empty_chat))
            // A side question asked before anything else still needs somewhere to show.
            .children(
                self.aside_row(self.store.conversations[self.store.active].id, window, cx)
                    .map(|row| div().w_full().max_w(COLUMN).px_6().mt_6().child(row)),
            )
    }

    pub(crate) fn page_view(&self, page: Page, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .flex_1()
            .px_8()
            .py_6()
            .gap_2()
            .child(
                div()
                    .text_xl()
                    .font_family(HEADING_FONT)
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(page.title()),
            )
            .child(
                div()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child(page.blurb()),
            )
    }

    /// Play a video a tool made, in the chat; where it cannot play here, in
    /// the system player. One plays at a time, and it stops speech playing.
    fn play_video(&mut self, path: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        use crate::platform::video::Event;
        self.stop_video(window);
        let playing = match crate::platform::video::play(&path) {
            Ok(playing) => playing,
            Err(_) => return cx.open_with_system(&path),
        };
        crate::platform::audio::stop();
        let (id, events) = (playing.id, playing.events.clone());
        self.video = Some(playing);
        cx.spawn_in(window, async move |this, cx| {
            while let Ok(event) = events.recv().await {
                let ended = matches!(event, Event::Ended(_));
                let current = this.update_in(cx, |this, window, cx| {
                    // What a stopped video still sends is dropped unseen.
                    if this.video.as_ref().is_none_or(|v| v.id != id) {
                        return false;
                    }
                    match event {
                        Event::Frame(image) => {
                            if let Some(old) = this.video_frame.replace(image) {
                                let _ = window.drop_image(old);
                            }
                        }
                        Event::Ended(why) => {
                            this.stop_video(window);
                            if why.is_some() {
                                this.error = why;
                            }
                        }
                    }
                    cx.notify();
                    true
                });
                if ended || !matches!(current, Ok(true)) {
                    break;
                }
            }
        })
        .detach();
        cx.notify();
    }

    /// Stop the video playing, and give its last frame back to the GPU.
    pub(crate) fn stop_video(&mut self, window: &mut Window) {
        self.video = None;
        if let Some(frame) = self.video_frame.take() {
            let _ = window.drop_image(frame);
        }
    }
}

/// Whether a retry set for `at` goes at `now`. Another session's reply owns
/// the agent while `busy`, and the retry waits for it rather than being lost.
fn retry_due(now: u64, at: u64, busy: bool) -> bool {
    now >= at && !busy
}

/// What is left of a wait, as a clock counts it down: 0:45, 3:05, 1:02:03.
fn countdown(seconds: u64) -> String {
    let (hours, minutes, seconds) = (seconds / 3600, seconds / 60 % 60, seconds % 60);
    if hours > 0 {
        format!("{hours}:{minutes:02}:{seconds:02}")
    } else {
        format!("{minutes}:{seconds:02}")
    }
}

/// The line under a reply. Cached input is a fraction of the price of fresh
/// input, so the hit rate is worth showing whenever the provider reports it.
fn usage_line(message: &Message) -> String {
    let mut parts = vec![format!("{} out", message.tokens)];
    if message.per_second > 0. {
        parts.push(format!("{:.1} tok/s", message.per_second));
    }
    let input = message.input_tokens();
    if input > 0 {
        parts.push(format!("{input} in"));
        parts.extend(message.fullness());
        if message.cached_tokens > 0 {
            let share = 100 * message.cached_tokens / input;
            parts.push(format!("{share}% cached"));
        }
    }
    parts.join(" · ")
}

/// The longest side a picture a tool made is decoded at: twice as wide as
/// it is shown, for a screen up to twice as sharp.
const MADE_SIDE: u32 = 720;

/// What a media tool made, shown as itself: a picture, a video by its first
/// frame until it plays here, a sound with a button to play it. Each gets
/// buttons to open the file or find it. `frame` is the video's newest frame
/// while `playing`.
fn made_file(
    index: usize,
    path: PathBuf,
    playing: bool,
    frame: Option<std::sync::Arc<RenderImage>>,
    cx: &mut Context<Chat>,
) -> impl IntoElement {
    let kind = |known: &[&str]| {
        path.extension()
            .is_some_and(|extension| known.iter().any(|k| extension.eq_ignore_ascii_case(k)))
    };
    let picture = kind(&["png", "jpg", "jpeg", "webp"]);
    let video = kind(&["mp4", "webm", "mov"]);
    let sound = kind(&["mp3", "wav", "ogg", "flac"]);
    let (open, reveal) = (path.clone(), path.clone());
    let theme = cx.theme();
    let (radius, tile, mark, on_mark) = (
        theme.radius,
        theme.secondary,
        theme.background.opacity(0.75),
        theme.foreground,
    );
    let still: ImageSource = if let Some(frame) = frame {
        frame.into()
    } else if video {
        let source = path.clone();
        ImageSource::from(move |window: &mut Window, cx: &mut App| {
            window.use_asset::<crate::platform::poster::Poster>(&source, cx)
        })
    } else {
        path.clone().into()
    };
    // Without a still, or until it comes, a video is a plain frame.
    let blank = move || {
        div()
            .w(px(320.))
            .h(px(180.))
            .rounded(radius)
            .bg(tile)
            .into_any_element()
    };
    v_flex()
        .ml_2()
        .pl_3()
        .gap_1()
        .items_start()
        .when(picture || video, |this| {
            let (open, play) = (path.clone(), path.clone());
            this.child(
                div()
                    .id(("made-picture", index))
                    .relative()
                    .cursor_pointer()
                    .child(
                        crate::ui::pictures::held_while_shown(&path, MADE_SIDE).child(
                            img(still)
                                .h(px(220.))
                                .max_w(px(360.))
                                .object_fit(ObjectFit::Contain)
                                .rounded(radius)
                                .when(video, |img| img.with_loading(blank).with_fallback(blank)),
                        ),
                    )
                    .when(video, |this| {
                        // Big in the middle to start it; small in the corner
                        // while it plays, out of the picture's way.
                        this.child(
                            div()
                                .absolute()
                                .inset_0()
                                .flex()
                                .map(|place| {
                                    if playing {
                                        place.items_end().justify_start().p_2()
                                    } else {
                                        place.items_center().justify_center()
                                    }
                                })
                                .child(
                                    div()
                                        .size(px(if playing { 28. } else { 44. }))
                                        .rounded_full()
                                        .bg(mark)
                                        .flex()
                                        .items_center()
                                        .justify_center()
                                        .child(
                                            Icon::new(if playing {
                                                AppIcon::Stop
                                            } else {
                                                AppIcon::Play
                                            })
                                            .map(|icon| {
                                                if playing {
                                                    icon.size_3p5()
                                                } else {
                                                    icon.size_5()
                                                }
                                            })
                                            .text_color(on_mark),
                                        ),
                                ),
                        )
                    })
                    .map(|this| {
                        if !video {
                            return this.on_click(move |_, _, cx| cx.open_with_system(&open));
                        }
                        this.on_click(cx.listener(move |chat, _, window, cx| {
                            if playing {
                                chat.stop_video(window);
                                cx.notify();
                            } else {
                                chat.play_video(play.clone(), window, cx);
                            }
                        }))
                    }),
            )
        })
        .child(
            h_flex()
                .gap_1()
                .when(sound, |this| {
                    this.child(sound_button(index, path.clone(), cx))
                })
                .child(
                    Button::new(("open-made", index))
                        .plain(cx)
                        .xsmall()
                        .label("Open")
                        .on_click(move |_, _, cx| cx.open_with_system(&open)),
                )
                .child(
                    Button::new(("reveal-made", index))
                        .plain(cx)
                        .xsmall()
                        .icon(Icon::new(AppIcon::Folder))
                        .tooltip("Show in folder")
                        .on_click(move |_, _, cx| cx.reveal_path(&reveal)),
                ),
        )
}

/// Play a sound a tool made, or stop it. While it plays, the chat looks every
/// quarter second whether it has ended, to turn the button back to Play.
fn sound_button(index: usize, path: PathBuf, cx: &mut Context<Chat>) -> Button {
    let playing = crate::platform::audio::playing(&path);
    Button::new(("play-made", index))
        .plain(cx)
        .xsmall()
        .icon(Icon::new(if playing {
            AppIcon::Stop
        } else {
            AppIcon::Play
        }))
        .label(if playing { "Stop" } else { "Play" })
        .on_click(cx.listener(move |this, _, window, cx| {
            if crate::platform::audio::playing(&path) {
                crate::platform::audio::stop();
            } else if let Err(why) = crate::platform::audio::play(&path) {
                this.error = Some(why);
            } else {
                this.stop_video(window);
                let sound = path.clone();
                cx.spawn(async move |this, cx| {
                    let quarter = std::time::Duration::from_millis(250);
                    loop {
                        cx.background_executor().timer(quarter).await;
                        let on = cx.update(|_| crate::platform::audio::playing(&sound));
                        if !on.unwrap_or(false) {
                            break;
                        }
                    }
                    let _ = this.update(cx, |_, cx| cx.notify());
                })
                .detach();
            }
            cx.notify();
        }))
}

#[cfg(test)]
mod tests {
    // Deliberately not `use super::*`: the prelude re-exports gpui, whose own
    // `test` macro would shadow the built-in attribute here.
    use super::{countdown, retry_due, usage_line};
    use letronna_core::chat::Message;

    #[test]
    fn a_retry_that_falls_due_while_busy_waits_rather_than_going() {
        assert!(!retry_due(999, 1_000, false));
        assert!(retry_due(1_000, 1_000, false));
        assert!(!retry_due(1_000, 1_000, true));
        assert!(retry_due(1_500, 1_000, false));
    }

    #[test]
    fn a_countdown_reads_as_a_clock_does() {
        assert_eq!(countdown(0), "0:00");
        assert_eq!(countdown(45), "0:45");
        assert_eq!(countdown(185), "3:05");
        assert_eq!(countdown(3_723), "1:02:03");
    }

    #[test]
    fn usage_line_reports_the_cache_hit_rate() {
        let message = Message {
            tokens: 16,
            per_second: 18.4,
            prompt_tokens: 3291,
            cached_tokens: 3200,
            ..Default::default()
        };
        assert_eq!(
            usage_line(&message),
            "16 out · 18.4 tok/s · 3291 in · 97% cached"
        );
        // A provider that reports no cache figure says nothing about caching.
        let uncached = Message {
            cached_tokens: 0,
            ..message.clone()
        };
        assert_eq!(usage_line(&uncached), "16 out · 18.4 tok/s · 3291 in");
    }

    #[test]
    fn usage_line_says_how_full_the_window_was_after_the_prompt() {
        let message = Message {
            tokens: 16,
            prompt_tokens: 76_000,
            cached_tokens: 72_200,
            window: 200_000,
            ..Default::default()
        };
        assert_eq!(
            usage_line(&message),
            "16 out · 76000 in · 38% of 200K · 95% cached"
        );
    }
}
