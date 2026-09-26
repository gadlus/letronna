use crate::prelude::*;

impl Chat {
    pub(crate) fn message_row(
        &self,
        index: usize,
        message: &Message,
        labeled: bool,
        streaming: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        if message.role == "tool" {
            return self.tool_row(index, message, window, cx).into_any_element();
        }
        let is_user = message.role == "user";
        let label: String = if is_user {
            "You".into()
        } else if message.model.is_empty() {
            self.store.settings.model.clone()
        } else {
            message.model.clone()
        };
        let leading = px(26.);
        let cx_muted = cx.theme().muted_foreground;
        let body: AnyElement = if is_user {
            TextView::markdown(("md", index), message.content.clone(), window, cx)
                .text(message.content.clone())
                .selectable(true)
                .line_height(leading)
                .into_any_element()
        } else if message.content.is_empty() && !message.tool_calls.is_empty() {
            // A turn that only asked for tools; the tool rows below say what happened.
            div().into_any_element()
        } else if message.content.is_empty() {
            h_flex()
                .gap_2()
                .items_center()
                .text_color(cx.theme().muted_foreground)
                .child(
                    gpui_component::spinner::Spinner::new()
                        .xsmall()
                        .color(cx.theme().primary),
                )
                .child("Thinking…")
                .into_any_element()
        } else {
            TextView::markdown(("md", index), message.content.clone(), window, cx)
                .selectable(true)
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
            .gap_2p5()
            .when(labeled, |this| {
                this.child(
                    h_flex()
                        .items_center()
                        .gap_2()
                        .text_xs()
                        .font_family(HEADING_FONT)
                        .child(
                            div()
                                .font_weight(FontWeight::MEDIUM)
                                .text_color(if is_user {
                                    cx.theme().muted_foreground
                                } else {
                                    cx.theme().primary
                                })
                                .child(label),
                        )
                        .when(streaming, |this| {
                            this.child(
                                div()
                                    .size(px(6.))
                                    .rounded_full()
                                    .bg(cx.theme().primary)
                                    .with_animation(
                                        "pulse",
                                        Animation::new(std::time::Duration::from_millis(1000))
                                            .repeat()
                                            .with_easing(ease_in_out),
                                        |dot, delta| {
                                            let wave = 1.0 - (delta * 2.0 - 1.0).abs();
                                            dot.opacity(0.25 + 0.75 * wave)
                                        },
                                    ),
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
            .child(div().w_full().overflow_hidden().pb_1().child(body))
            .when(
                !is_user && message.tokens > 0 && !message.content.is_empty(),
                |this| {
                    this.child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(usage_line(message)),
                    )
                },
            )
            .with_animation(
                ("appear", index),
                Animation::new(std::time::Duration::from_millis(260))
                    .with_easing(gpui::ease_out_quint()),
                |el, delta| el.opacity(delta).mt(px(10. * (1. - delta))),
            )
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
        let open = self.thinking_open.contains(&index);
        let summary = letronna_core::tools::summary(&message.name, &message.args);
        let lines = message.content.lines().count();
        let content = message.content.clone();
        let one_line = letronna_core::tools::one_line;
        // A file a media tool made, while it is still where it was saved.
        let made = (!message.is_error)
            .then(|| letronna_core::media::saved(&message.content))
            .flatten()
            .filter(|path| path.exists());
        let (status, color) = if message.is_error {
            (one_line(&message.content), cx.theme().danger)
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
            (format!("{lines} lines"), cx.theme().muted_foreground)
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
                    .hover(|s| s.bg(cx.theme().secondary.opacity(0.5)))
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
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if !this.thinking_open.remove(&index) {
                            this.thinking_open.insert(index);
                        }
                        cx.notify();
                    })),
            )
            .when_some(made, |this, path| this.child(made_file(index, path, cx)))
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
                            ("tool-out", index),
                            letronna_core::tools::clamp_lines(&content, 400),
                            px(220.),
                            window,
                            cx,
                        )),
                )
            })
    }

    fn thinking_block(
        &self,
        index: usize,
        message: &Message,
        streaming: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let open = streaming || self.thinking_open.contains(&index);
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
                    .hover(|s| s.bg(cx.theme().secondary.opacity(0.5)))
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
                        if this.thinking_open.contains(&index) {
                            this.thinking_open.remove(&index);
                        } else {
                            this.thinking_open.insert(index);
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
                            TextView::markdown(("reason", index), reasoning, window, cx)
                                .selectable(true)
                                .line_height(px(21.)),
                        ),
                )
            })
    }

    pub(crate) fn empty_state(&self, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
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
}

/// The line under a reply. Cached input is a fraction of the price of fresh
/// input, so the hit rate is worth showing whenever the provider reports it.
fn usage_line(message: &Message) -> String {
    let mut parts = vec![format!("{} out", message.tokens)];
    if message.per_second > 0. {
        parts.push(format!("{:.1} tok/s", message.per_second));
    }
    if message.prompt_tokens > 0 {
        parts.push(format!("{} in", message.prompt_tokens));
        if message.cached_tokens > 0 {
            let share = 100 * message.cached_tokens / message.prompt_tokens;
            parts.push(format!("{share}% cached"));
        }
    }
    parts.join(" · ")
}

/// What a media tool made, shown as itself: a picture, a video by its first
/// frame (the system player plays it), a sound with a button to play it here.
/// Each gets buttons to open the file or find it.
fn made_file(index: usize, path: PathBuf, cx: &mut Context<Chat>) -> impl IntoElement {
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
    let still: ImageSource = if video {
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
            let open = path.clone();
            this.child(
                div()
                    .id(("made-picture", index))
                    .relative()
                    .cursor_pointer()
                    .child(
                        img(still)
                            .h(px(220.))
                            .max_w(px(360.))
                            .object_fit(ObjectFit::Contain)
                            .rounded(radius)
                            .when(video, |img| img.with_loading(blank).with_fallback(blank)),
                    )
                    .when(video, |this| {
                        this.child(
                            div()
                                .absolute()
                                .inset_0()
                                .flex()
                                .items_center()
                                .justify_center()
                                .child(
                                    div()
                                        .size(px(44.))
                                        .rounded_full()
                                        .bg(mark)
                                        .flex()
                                        .items_center()
                                        .justify_center()
                                        .child(
                                            Icon::new(AppIcon::Play).size_5().text_color(on_mark),
                                        ),
                                ),
                        )
                    })
                    .on_click(move |_, _, cx| cx.open_with_system(&open)),
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
                        .ghost()
                        .xsmall()
                        .label("Open")
                        .on_click(move |_, _, cx| cx.open_with_system(&open)),
                )
                .child(
                    Button::new(("reveal-made", index))
                        .ghost()
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
        .ghost()
        .xsmall()
        .icon(Icon::new(if playing {
            AppIcon::Stop
        } else {
            AppIcon::Play
        }))
        .label(if playing { "Stop" } else { "Play" })
        .on_click(cx.listener(move |this, _, _, cx| {
            if crate::platform::audio::playing(&path) {
                crate::platform::audio::stop();
            } else if let Err(why) = crate::platform::audio::play(&path) {
                this.error = Some(why);
            } else {
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
    use super::usage_line;
    use letronna_core::chat::Message;

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
}
