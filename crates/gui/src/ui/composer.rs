use crate::prelude::*;

impl Chat {
    /// The (gateway, model) pairs the picker shows for what is typed in its
    /// search box.
    pub(crate) fn model_matches(&self, cx: &App) -> Vec<(String, String)> {
        let current = self.model.read(cx).value().to_string();
        let query = self.model_search.read(cx).value().to_lowercase();
        let on = &self.store.settings.gateway;
        let mut ids = self.models.clone();
        // The model in use stays on the list even when its gateway did not
        // offer it, or the picker could not show what is picked.
        if !current.is_empty() && !ids.iter().any(|(g, m)| g == on && *m == current) {
            ids.insert(0, (on.clone(), current));
        }
        ids.retain(|(gw, id)| {
            query.is_empty()
                || id.to_lowercase().contains(&query)
                || gw.to_lowercase().contains(&query)
        });
        ids
    }

    pub(crate) fn model_picker(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let current = self.model.read(cx).value().to_string();
        let short = current.rsplit('/').next().unwrap_or(&current).to_string();
        let mut list = v_flex()
            .id("model-list")
            .max_h(px(320.))
            .overflow_y_scroll()
            .p_1()
            .gap_0p5();
        let on = self.store.settings.gateway.clone();
        let matches = self.model_matches(cx);
        if matches.is_empty() {
            list = list.child(
                div()
                    .px_2()
                    .py_1p5()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(if self.loading_models {
                        "Loading models…"
                    } else if self.models.is_empty() {
                        "No models yet. Set up a gateway in Settings."
                    } else {
                        "No models match"
                    }),
            );
        }
        for (i, (gw, id)) in matches.into_iter().enumerate() {
            let active = gw == on && id == current;
            let (pick_gw, pick) = (gw.clone(), id.clone());
            list = list.child(
                Button::new(("model", i))
                    .ghost()
                    .xsmall()
                    .w_full()
                    .justify_start()
                    .child(
                        h_flex()
                            .w_full()
                            .gap_2()
                            .justify_between()
                            .child(
                                div()
                                    .truncate()
                                    .when(active, |d| d.text_color(cx.theme().primary))
                                    .child(id),
                            )
                            .child(
                                div()
                                    .flex_shrink_0()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(gw),
                            ),
                    )
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.choose_model(pick_gw.clone(), pick.clone(), window, cx)
                    })),
            );
        }
        div()
            .relative()
            .flex_shrink_0()
            .child(
                Button::new("model-trigger")
                    .ghost()
                    .xsmall()
                    .text_color(cx.theme().muted_foreground)
                    .child(
                        h_flex()
                            .gap_1()
                            .items_center()
                            .child(short)
                            .child(Icon::new(IconName::ChevronDown).size_3()),
                    )
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.model_open = !this.model_open;
                        if this.model_open {
                            this.model_search.update(cx, |s, cx| {
                                s.set_value("", window, cx);
                                s.focus(window, cx);
                            });
                        }
                        cx.notify();
                    })),
            )
            .when(self.model_open, |this| {
                this.child(
                    div().absolute().top_0().right_0().child(
                        deferred(
                            anchored()
                                .anchor(Corner::BottomRight)
                                .snap_to_window_with_margin(px(8.))
                                .child(
                                    div()
                                        .occlude()
                                        .w(px(340.))
                                        .pb_1p5()
                                        .child(
                                            v_flex()
                                                .bg(cx.theme().background)
                                                .border_1()
                                                .border_color(cx.theme().border)
                                                .rounded(cx.theme().radius_lg)
                                                .shadow_md()
                                                .child(
                                                    div()
                                                        .p_1()
                                                        .border_b_1()
                                                        .border_color(cx.theme().border)
                                                        .child(
                                                            Input::new(&self.model_search)
                                                                .xsmall()
                                                                .appearance(false)
                                                                .prefix(
                                                                    Icon::new(AppIcon::Search)
                                                                        .size_3()
                                                                        .text_color(
                                                                            cx.theme()
                                                                                .muted_foreground,
                                                                        ),
                                                                ),
                                                        ),
                                                )
                                                .child(list),
                                        )
                                        .on_mouse_down_out(cx.listener(|this, _, _, cx| {
                                            this.model_open = false;
                                            cx.notify();
                                        })),
                                ),
                        )
                        .with_priority(1),
                    ),
                )
            })
    }

    pub(crate) fn attach(
        &mut self,
        directories: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let receiver = cx.prompt_for_paths(PathPromptOptions {
            files: !directories,
            directories,
            multiple: true,
            prompt: None,
        });
        cx.spawn_in(window, async move |this, cx| {
            if let Ok(Ok(Some(paths))) = receiver.await {
                let _ = this.update_in(cx, |this, window, cx| {
                    let mut text = String::new();
                    for path in paths {
                        text.push_str(&format!("[attached: {}]\n", path.display()));
                    }
                    this.insert(&text, window, cx);
                });
            }
        })
        .detach();
    }

    /// Pick the project folder for the active session. Tools resolve paths here.
    pub(crate) fn attach_workspace(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let receiver = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: None,
        });
        let id = self.store.conversations[self.store.active].id;
        cx.spawn_in(window, async move |this, cx| {
            if let Ok(Ok(Some(paths))) = receiver.await {
                let _ = this.update(cx, |this, cx| {
                    if let Some(ix) = this.store.index_of(id) {
                        this.store.conversations[ix].workspace = paths.into_iter().next();
                        this.save();
                        cx.notify();
                    }
                });
            }
        })
        .detach();
    }

    /// Put the cursor in the composer, so the next thing typed is a message.
    /// Not while a dialog is up: the keys are its until it closes.
    pub(crate) fn focus_composer(&self, window: &mut Window, cx: &mut Context<Self>) {
        if window.has_active_dialog(cx) {
            return;
        }
        self.composer
            .update(cx, |input, cx| input.focus(window, cx));
    }

    pub(crate) fn insert(&mut self, text: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.composer.update(cx, |input, cx| {
            let mut value = input.value().to_string();
            value.push_str(text);
            input.set_value(value, window, cx);
        });
    }

    /// The model's checklist, above the composer while work is outstanding.
    /// It disappears on its own once everything is done.
    pub(crate) fn todo_panel(&self, cx: &mut Context<Self>) -> Option<impl IntoElement> {
        let todos = &self.store.conversations[self.store.active].todos;
        if todos.is_empty() || todos.iter().all(|t| t.status == "done") {
            return None;
        }
        let done = todos.iter().filter(|t| t.status == "done").count();
        let rows = todos.iter().enumerate().map(|(i, todo)| {
            // The three states use icons from different sets, so build the
            // element in each arm rather than passing a name around.
            let (icon, color) = match todo.status.as_str() {
                "done" => (Icon::new(IconName::CircleCheck), cx.theme().green),
                "doing" => (Icon::new(IconName::LoaderCircle), cx.theme().primary),
                _ => (Icon::new(AppIcon::Circle), cx.theme().muted_foreground),
            };
            h_flex()
                .gap_2()
                .items_center()
                .text_xs()
                .child(icon.size_3().text_color(color))
                .child(
                    div()
                        .when(todo.status == "done", |d| {
                            d.line_through().text_color(cx.theme().muted_foreground)
                        })
                        .child(todo.text.clone()),
                )
                .id(("todo", i))
        });
        Some(
            v_flex()
                .w_full()
                .gap_1p5()
                .mb_2()
                .px_3()
                .py_2p5()
                .rounded(cx.theme().radius)
                .border_1()
                .border_color(cx.theme().border)
                .bg(cx.theme().secondary.opacity(0.3))
                .child(
                    div()
                        .text_xs()
                        .font_family(HEADING_FONT)
                        .text_color(cx.theme().muted_foreground)
                        .child(format!("Plan · {done} of {} done", todos.len())),
                )
                .children(rows),
        )
    }

    pub(crate) fn composer(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let chat = cx.entity();
        let attach = Button::new("attach")
            .ghost()
            .xsmall()
            .icon(Icon::new(IconName::Plus))
            .disabled(self.busy)
            .dropdown_menu(move |menu, _, _| {
                let files = chat.clone();
                let folder = chat.clone();
                let images = chat.clone();
                let snippet = chat.clone();
                menu.min_w(px(180.))
                    .item(
                        PopupMenuItem::new("Files")
                            .icon(Icon::new(AppIcon::File))
                            .on_click(move |_, window, cx| {
                                files.update(cx, |this, cx| this.attach(false, window, cx))
                            }),
                    )
                    .item(
                        PopupMenuItem::new("Project folder")
                            .icon(Icon::new(AppIcon::Folder))
                            .on_click(move |_, window, cx| {
                                folder.update(cx, |this, cx| this.attach_workspace(window, cx))
                            }),
                    )
                    .item(
                        PopupMenuItem::new("Images")
                            .icon(Icon::new(AppIcon::Image))
                            .on_click(move |_, window, cx| {
                                images.update(cx, |this, cx| this.attach(false, window, cx))
                            }),
                    )
                    .separator()
                    .item(
                        PopupMenuItem::new("Prompt snippet")
                            .icon(Icon::new(AppIcon::FileText))
                            .on_click(move |_, window, cx| {
                                snippet.update(cx, |this, cx| {
                                    this.insert(
                                        "Goal:\nContext:\nConstraints:\nDeliverable:\n",
                                        window,
                                        cx,
                                    )
                                })
                            }),
                    )
            });
        h_flex()
            .w_full()
            .items_center()
            .gap_1()
            .rounded(cx.theme().radius)
            .border_1()
            .border_color(cx.theme().border)
            .bg(cx.theme().secondary.opacity(0.4))
            .px_2()
            .py_1()
            .child(attach)
            // Open while a reply streams, so the next message can be written
            // in the meantime; `send` holds it until the reply is done.
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .child(Input::new(&self.composer).w_full().appearance(false)),
            )
            .child(
                Button::new("plan-mode")
                    .ghost()
                    .xsmall()
                    .tooltip(if self.plan_mode {
                        "Plan mode on: reads and asks only. Click to let it act."
                    } else {
                        "Plan mode off. Click to stop it writing or running anything."
                    })
                    .text_color(if self.plan_mode {
                        cx.theme().primary
                    } else {
                        cx.theme().muted_foreground
                    })
                    .child(
                        h_flex()
                            .gap_1()
                            .items_center()
                            .child(Icon::new(AppIcon::Checklist).size_3())
                            .child("Plan"),
                    )
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.plan_mode = !this.plan_mode;
                        cx.notify();
                    })),
            )
            // Searching is a setting, but it changes what an answer is made
            // of, so it belongs where the answer is asked for rather than
            // three clicks away in a dialog.
            .child(
                Button::new("web-search")
                    .ghost()
                    .xsmall()
                    .tooltip(if self.store.settings.search {
                        "Web search on: it may look things up and cite them. Click to stop."
                    } else {
                        "Web search off. Click to let it look things up."
                    })
                    .text_color(if self.store.settings.search {
                        cx.theme().primary
                    } else {
                        cx.theme().muted_foreground
                    })
                    .child(
                        h_flex()
                            .gap_1()
                            .items_center()
                            .child(Icon::new(AppIcon::Search).size_3())
                            .child("Web"),
                    )
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.store.settings.search = !this.store.settings.search;
                        this.save();
                        cx.notify();
                    })),
            )
            .child(self.model_picker(cx))
            .child(
                Button::new("mic")
                    .ghost()
                    .xsmall()
                    .icon(Icon::new(AppIcon::Mic))
                    .tooltip("Voice input, coming soon"),
            )
            .child(if self.busy {
                // A turn can wait on a slow tool or a card nobody answers;
                // this is the way out that does not need a restart.
                Button::new("stop")
                    .primary()
                    .xsmall()
                    .rounded_full()
                    .icon(Icon::new(IconName::CircleX))
                    .tooltip_with_action("Stop", &Dismiss, None)
                    .on_click(cx.listener(|this, _, _, cx| this.stop_turn(cx)))
            } else {
                Button::new("send")
                    .primary()
                    .xsmall()
                    .rounded_full()
                    .icon(Icon::new(IconName::ArrowUp))
                    .tooltip("Send (Enter)")
                    .on_click(cx.listener(|this, _, window, cx| this.send(window, cx)))
            })
    }
}
