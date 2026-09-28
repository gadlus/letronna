use crate::prelude::*;

impl Chat {
    /// An id for part of a message that also names its session, so state
    /// kept by id -- a reply's parsed markdown, its fade-in -- never shows
    /// in another session with a message at the same place.
    pub(crate) fn row_id(&self, name: &str, index: usize) -> ElementId {
        let session = self.store.conversations[self.store.active].id;
        (SharedString::from(format!("{name}-{session}")), index).into()
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
            self.store.settings.model.clone()
        } else {
            message.model.clone()
        };
        let leading = px(24.);
        let cx_muted = cx.theme().muted_foreground;
        let md = self.row_id("md", index);
        // Not while a dialog is up: the text view starts a selection on any
        // press inside it, even one on a dialog drawn over it, and that press
        // never ends -- confirming a rewind left the reply half selected.
        let selectable = !window.has_active_dialog(cx);
        let body: AnyElement = if editing {
            v_flex()
                .gap_2()
                .child(Input::new(&self.edit_message).appearance(false))
                .child(
                    h_flex()
                        .gap_2()
                        .justify_end()
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
                                .label("Send")
                                .on_click(
                                    cx.listener(|this, _, window, cx| this.send_edit(window, cx)),
                                ),
                        ),
                )
                .into_any_element()
        } else if is_user {
            TextView::markdown(md, message.content.clone(), window, cx)
                .selectable(selectable)
                .text_size(px(15.))
                .font_weight(FontWeight::MEDIUM)
                .line_height(leading)
                .into_any_element()
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
            .when(!tool_only, |this| {
                this.child(div().w_full().overflow_hidden().pb_1().child(body))
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

    /// A tool result: one quiet line, click to see the output.
    fn tool_row(
        &self,
        index: usize,
        message: &Message,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let key = self.open_key(index);
        let open = self.thinking_open.contains(&key);
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
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if !this.thinking_open.remove(&key) {
                            this.thinking_open.insert(key);
                        }
                        cx.notify();
                    })),
            )
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
                            TextView::markdown(self.row_id("reason", index), reasoning, window, cx)
                                .selectable(!window.has_active_dialog(cx))
                                .line_height(px(21.)),
                        ),
                )
            })
    }

    pub(crate) fn empty_state(&self, cx: &mut Context<Self>) -> impl IntoElement {
        // Without a project folder the tools have nowhere to read or write,
        // and the only other place that says so is a chip in the status bar.
        let no_project = self.store.conversations[self.store.active]
            .workspace
            .is_none();
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
    fn stop_video(&mut self, window: &mut Window) {
        self.video = None;
        if let Some(frame) = self.video_frame.take() {
            let _ = window.drop_image(frame);
        }
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
        if message.cached_tokens > 0 {
            let share = 100 * message.cached_tokens / input;
            parts.push(format!("{share}% cached"));
        }
    }
    parts.join(" · ")
}

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
                        img(still)
                            .h(px(220.))
                            .max_w(px(360.))
                            .object_fit(ObjectFit::Contain)
                            .rounded(radius)
                            .when(video, |img| img.with_loading(blank).with_fallback(blank)),
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
