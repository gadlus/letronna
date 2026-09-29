use crate::prelude::*;
use gpui_component::{Selectable, menu::PopupMenu, popover::Popover};
use letronna_core::gate::Mode;

actions!(composer, [LeaveComposer]);

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
        // The chat models, unless a media tab is showing instead.
        let chat = self.model_kind == "chat";
        let matches = if chat {
            self.model_matches(cx)
        } else {
            Vec::new()
        };
        if chat && matches.is_empty() {
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
                    .plain(cx)
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
                                    .when(active, |d| d.text_color(cx.theme().link))
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
        if !chat {
            let query = self.model_search.read(cx).value().to_lowercase();
            list = self.media_list(list, &query, cx);
        }
        div()
            .relative()
            .flex_shrink_0()
            .child(
                Button::new("model-trigger")
                    .plain(cx)
                    .xsmall()
                    .text_color(cx.theme().muted_foreground)
                    .child(
                        h_flex()
                            .gap_1()
                            .items_center()
                            // A long model name gives way before the text does.
                            .child(div().max_w(px(160.)).truncate().child(short))
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
                                                .when(!self.media_models.is_empty(), |this| {
                                                    this.child(self.kind_tabs(cx))
                                                })
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

    /// Text, and each kind of media model there is, as tabs over the list.
    fn kind_tabs(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let there = |kind: &str| self.media_models.iter().any(|o| o.kind == kind);
        let kinds = std::iter::once(("chat", "Text")).chain(
            letronna_core::media::KINDS
                .into_iter()
                .filter(|(k, _)| there(k)),
        );
        let mut tabs = h_flex()
            .p_1()
            .gap_0p5()
            .flex_wrap()
            .border_b_1()
            .border_color(cx.theme().border);
        for (i, (kind, label)) in kinds.enumerate() {
            tabs = tabs.child(
                Button::new(("model-kind", i))
                    .plain(cx)
                    .xsmall()
                    .label(label)
                    .selected(self.model_kind == kind)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.model_kind = kind;
                        cx.notify();
                    })),
            );
        }
        tabs
    }

    /// The models of the media kind the menu shows, cheapest first, under a
    /// row that leaves the choice to Letronna. Picking one makes it the model
    /// that kind's tool uses.
    fn media_list(
        &self,
        mut list: Stateful<Div>,
        query: &str,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let kind = self.model_kind;
        let chosen = self.media_choices.get(kind);
        let auto = std::iter::once((None, "Auto".to_owned(), "the cheapest working".to_owned()));
        let offers = self
            .media_models
            .iter()
            .filter(|o| o.kind == kind)
            .filter(|o| {
                query.is_empty()
                    || o.id.to_lowercase().contains(query)
                    || o.maker.to_lowercase().contains(query)
            })
            .map(|o| (Some(o.id.clone()), o.id.clone(), o.price()));
        for (i, (pick, name, note)) in auto.chain(offers).enumerate() {
            let active = pick.as_ref() == chosen;
            list =
                list.child(
                    Button::new(("media-model", i))
                        .plain(cx)
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
                                        .when(active, |d| d.text_color(cx.theme().link))
                                        .child(name),
                                )
                                .child(
                                    div()
                                        .flex_shrink_0()
                                        .text_color(cx.theme().muted_foreground)
                                        .child(note),
                                ),
                        )
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.choose_media(kind, pick.clone(), cx)
                        })),
                );
        }
        list
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
                        let folder = paths.into_iter().next();
                        // A call already waiting in this session runs there too.
                        if this.streaming_id == Some(id)
                            && let Some(turn) = this.turn.as_mut()
                        {
                            turn.connection.workspace = folder.clone();
                        }
                        this.store.conversations[ix].workspace = folder;
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
        // A page without the composer takes the keys instead; a composer
        // that is not drawn would take them nowhere, shortcuts included.
        if self.page != Page::Chat {
            window.focus(&self.page_focus);
            return;
        }
        self.composer
            .update(cx, |input, cx| input.focus(window, cx));
    }

    /// Keep what was typed for the session the composer was showing, and
    /// bring back what was typed for the one on screen now.
    pub(crate) fn swap_draft(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let on_screen = self.store.conversations[self.store.active].id;
        if self.draft_for == on_screen {
            return;
        }
        let draft = self.composer.read(cx).value().to_string();
        // Unless that session was just deleted.
        if !draft.trim().is_empty() && self.store.index_of(self.draft_for).is_some() {
            self.drafts.insert(self.draft_for, draft);
        }
        self.draft_for = on_screen;
        let draft = self.drafts.remove(&on_screen).unwrap_or_default();
        self.composer.update(cx, |input, cx| {
            // Inserted rather than set, so the caret is after it and typing
            // carries on where it left off.
            input.set_value("", window, cx);
            input.insert(draft, window, cx);
        });
    }

    pub(crate) fn insert(&mut self, text: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.composer.update(cx, |input, cx| {
            let mut value = input.value().to_string();
            value.push_str(text);
            input.set_value(value, window, cx);
        });
    }

    /// The session a reply is running in, when that is not the one on screen.
    pub(crate) fn busy_elsewhere(&self) -> Option<u64> {
        let on_screen = self.store.conversations[self.store.active].id;
        self.streaming_id.filter(|&id| self.busy && id != on_screen)
    }

    /// While a reply runs in another session this one cannot send, so say
    /// where it is and whether it waits on the user, with a way there.
    pub(crate) fn busy_note(&self, cx: &mut Context<Self>) -> Option<impl IntoElement> {
        let id = self.busy_elsewhere()?;
        let title = &self.store.conversations[self.store.index_of(id)?].title;
        let waiting = self.turn.as_ref().is_some_and(|turn| {
            matches!(turn.next(|_| letronna_core::gate::Outcome::Ask),
                letronna_core::agent::Next::AskUser(call) if self.decide(&call, &self.rules) == letronna_core::gate::Outcome::Ask)
        });
        let text = if waiting {
            format!("\u{201c}{title}\u{201d} is waiting on you. Answer it there, then send here.")
        } else {
            format!("Letronna is replying in \u{201c}{title}\u{201d}. Send here once it is done.")
        };
        Some(
            h_flex()
                .gap_2()
                .items_center()
                .pb_2()
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .child(div().flex_1().min_w_0().truncate().child(text))
                .child(
                    Button::new("open-busy")
                        .plain(cx)
                        .xsmall()
                        .label("Open it")
                        .on_click(cx.listener(move |this, _, window, cx| {
                            // Looked up again: sessions may have moved since.
                            let Some(index) = this.store.index_of(id) else {
                                return;
                            };
                            this.store.active = index;
                            this.error = None;
                            this.page = Page::Chat;
                            this.follow();
                            this.focus_composer(window, cx);
                            this.save();
                            cx.notify();
                        })),
                ),
        )
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

    /// The session's mode, which opens the list of modes to pick from on a
    /// click, or on Enter or Space while it has the focus.
    fn mode_chip(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let (chat, mode) = (cx.entity(), self.active_mode());
        let icon = if mode == Mode::Plan {
            AppIcon::Checklist
        } else {
            AppIcon::Shield
        };
        let chip = Button::new("mode")
            .ghost()
            .xsmall()
            .tooltip(format!(
                "{} mode: {}. Click to change.",
                mode.label(),
                mode.about()
            ))
            .text_color(if mode == Mode::Default {
                cx.theme().muted_foreground
            } else {
                cx.theme().primary
            })
            .child(
                h_flex()
                    .gap_1()
                    .items_center()
                    .child(Icon::new(icon).size_3())
                    .child(mode.label()),
            )
            // The popover opens on a mouse press only, so a key opens it here.
            .on_click(cx.listener(|this, event: &ClickEvent, window, cx| {
                if event.is_keyboard() {
                    this.open_mode_menu(window, cx);
                }
            }));
        Popover::new("mode-menu")
            .appearance(false)
            .overlay_closable(false)
            .open(self.mode_menu.is_some())
            .on_open_change(move |open, window, cx| {
                chat.update(cx, |this, cx| {
                    if *open {
                        this.open_mode_menu(window, cx);
                    } else {
                        this.mode_menu = None;
                        cx.notify();
                    }
                })
            })
            .trigger(chip)
            .when_some(self.mode_menu.clone(), |popover, menu| {
                popover.content(move |_, _, _| menu.clone())
            })
    }

    /// Open the list of modes with the keys in it, as the chip's popover
    /// shows it.
    fn open_mode_menu(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.mode_menu.is_some() {
            return;
        }
        let (chat, mode) = (cx.entity(), self.active_mode());
        let menu = PopupMenu::build(window, cx, move |menu, _, _| {
            Mode::OFFERED.into_iter().fold(menu, |menu, choice| {
                let chat = chat.clone();
                menu.item(
                    PopupMenuItem::new(choice.label())
                        .checked(choice == mode)
                        .on_click(move |_, _, cx| {
                            chat.update(cx, |this, cx| this.set_mode(choice, cx))
                        }),
                )
            })
        });
        menu.focus_handle(cx).focus(window);
        cx.subscribe(&menu, |this, _, _: &DismissEvent, cx| {
            this.mode_menu = None;
            cx.notify();
        })
        .detach();
        self.mode_menu = Some(menu);
        cx.notify();
    }

    /// The box a message is written in, with its controls. `compact` puts
    /// the controls under the text, for a narrow pane.
    pub(crate) fn composer(&self, compact: bool, cx: &mut Context<Self>) -> impl IntoElement {
        let chat = cx.entity();
        let attach = Button::new("attach")
            .plain(cx)
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
        // Open while a reply streams, so the next message can be written
        // in the meantime; `send` holds it until the reply is done.
        let input = div()
            .min_w_0()
            .key_context("Composer")
            // Ctrl+Enter sends now what Enter would queue for the next step.
            // With nothing running it stays the new line it always was.
            .on_action(cx.listener(|this, _: &SendNow, window, cx| {
                if !this.busy {
                    return cx.propagate();
                }
                this.send(window, cx);
                this.send_now(cx);
            }))
            .child(Input::new(&self.composer).w_full().appearance(false));
        let controls = h_flex()
            .items_center()
            .gap_1()
            // The session's mode sits where the message is written, since it
            // decides what the reply may do without asking.
            .child(self.mode_chip(cx))
            // Searching is a setting, but it changes what an answer is made
            // of, so it belongs where the answer is asked for rather than
            // three clicks away in a dialog.
            .child(
                Button::new("web-search")
                    .plain(cx)
                    .xsmall()
                    .tooltip(if self.store.settings.search {
                        "Web search on: it may look things up and cite them. Click to stop."
                    } else {
                        "Web search off. Click to let it look things up."
                    })
                    .selected(self.store.settings.search)
                    .text_color(if self.store.settings.search {
                        cx.theme().link
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
                    .plain(cx)
                    .xsmall()
                    .icon(Icon::new(AppIcon::Mic))
                    .disabled(true)
                    .tooltip("Voice input, coming soon"),
            )
            // Stop only where the reply is: in another session it would end
            // a reply that is not on screen.
            .when(self.busy && self.busy_elsewhere().is_none(), |row| {
                // A turn can wait on a slow tool or a card nobody answers;
                // this is the way out that does not need a restart.
                row.child(
                    Button::new("stop")
                        .ghost()
                        .xsmall()
                        .rounded_full()
                        .icon(Icon::new(IconName::CircleX))
                        .tooltip_with_action("Stop", &Dismiss, None)
                        .on_click(cx.listener(|this, _, _, cx| this.stop_turn(cx))),
                )
            })
            // While a reply runs, sending queues the message for its next
            // step, as in Claude Code, rather than doing nothing.
            .child(
                Button::new("send")
                    .primary()
                    .xsmall()
                    .rounded_full()
                    .icon(Icon::new(IconName::ArrowUp))
                    .tooltip(if self.busy {
                        "Send after this step (Enter)"
                    } else {
                        "Send (Enter)"
                    })
                    .disabled(self.composer_empty || self.busy_elsewhere().is_some())
                    .on_click(cx.listener(|this, _, window, cx| this.send(window, cx))),
            );
        let frame = if compact {
            v_flex()
        } else {
            h_flex().items_center()
        }
        .w_full()
        .gap_1()
        .rounded(cx.theme().radius)
        .border_1()
        .border_color(cx.theme().border)
        .bg(cx.theme().secondary.opacity(0.4))
        .px_2()
        .py_1();
        // Narrow, the text takes the whole width and the controls go under
        // it, rather than the text being squeezed to a few words a line.
        if compact {
            frame.child(input.w_full()).child(
                h_flex()
                    .items_center()
                    .gap_1()
                    .child(attach)
                    .child(div().flex_1())
                    .child(controls),
            )
        } else {
            frame.child(attach).child(input.flex_1()).child(controls)
        }
    }
}
