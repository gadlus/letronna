use crate::prelude::*;
use gpui_component::{Selectable, menu::PopupMenu, popover::Popover};
use letronna_core::{crew, gate::Mode};

actions!(composer, [LeaveComposer, KeysOrType]);

/// The modes the window offers. Auto waits until the window has its judge:
/// here it would be Accept edits under a label that promises more.
pub(crate) fn offered() -> impl Iterator<Item = Mode> {
    Mode::OFFERED.into_iter().filter(|mode| *mode != Mode::Auto)
}

/// How the window shows `mode`: Auto as Accept edits, which is what it does
/// here until the window has its judge, so the list has it ticked.
pub(crate) fn shown(mode: Mode) -> Mode {
    if mode == Mode::Auto {
        Mode::AcceptEdits
    } else {
        mode
    }
}

impl Chat {
    /// The (gateway, model) pairs the picker shows for what is typed in its
    /// search box.
    pub(crate) fn model_matches(&self, cx: &App) -> Vec<(String, String)> {
        self.models_named(&self.model_search.read(cx).value())
    }

    /// The pairs whose gateway or id contains `query`, the model in use
    /// kept on the list.
    pub(crate) fn models_named(&self, query: &str) -> Vec<(String, String)> {
        let (on, current) = self.store.model(self.store.active);
        let (on, current) = (on.to_owned(), current.to_owned());
        let query = query.to_lowercase();
        let mut ids = self.models.clone();
        // The model in use stays on the list even when its gateway did not
        // offer it, or the picker could not show what is picked.
        if !current.is_empty() && !ids.iter().any(|(g, m)| *g == on && *m == current) {
            ids.insert(0, (on, current));
        }
        ids.retain(|(gw, id)| {
            query.is_empty()
                || id.to_lowercase().contains(&query)
                || gw.to_lowercase().contains(&query)
        });
        ids
    }

    pub(crate) fn model_picker(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let (on, current) = self.store.model(self.store.active);
        let (on, current) = (on.to_owned(), current.to_owned());
        let short = current.rsplit('/').next().unwrap_or(&current).to_string();
        let mut list = v_flex()
            .id("model-list")
            .max_h(px(320.))
            .overflow_y_scroll()
            .p_1()
            .gap_0p5();
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
                    // As references, which sending expands into the message
                    // the way a typed `@path` is.
                    let workspace = this.store.conversations[this.store.active]
                        .workspace
                        .clone();
                    let mut text = String::new();
                    for path in paths {
                        let named =
                            letronna_core::references::reference(&path, workspace.as_deref());
                        text.push_str(&format!("{named} "));
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

    /// A key that typed nothing where the focus is, on a turn, the sidebar
    /// or the panel, starts a message instead, as long as it is a character.
    /// Not on another page, which has no composer to take it.
    pub(crate) fn type_into_composer(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.page != Page::Chat || self.settings_open || window.has_active_dialog(cx) {
            return;
        }
        // A text box, a menu or the browser's page has the key already.
        let taken = window.context_stack().iter().any(|context| {
            ["Input", "PopupMenu", "Browser"]
                .iter()
                .any(|name| context.contains(name))
        });
        let Some(text) = typed(&event.keystroke).filter(|_| !taken) else {
            return;
        };
        let text = text.to_owned();
        self.region = None;
        self.focus_composer(window, cx);
        self.composer
            .update(cx, |input, cx| input.insert(text, window, cx));
        cx.stop_propagation();
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
        self.crew_reset(cx);
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
                            this.leave_for(index);
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

    /// The session's standing goal, above the composer while it has one:
    /// what it is, how many turns it has carried on for, and whether it is
    /// being looked at now.
    pub(crate) fn goal_line(&self, cx: &mut Context<Self>) -> Option<impl IntoElement> {
        let goal = self.store.conversations[self.store.active].goal.as_ref()?;
        let mut text = goal.line();
        if self.checking_goal() {
            text.push_str(" · checking");
        }
        Some(
            h_flex()
                .gap_1p5()
                .items_center()
                .pb_2()
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .child(
                    Icon::new(IconName::CircleCheck)
                        .size_3()
                        .text_color(cx.theme().primary),
                )
                .child(div().flex_1().min_w_0().truncate().child(text)),
        )
    }

    /// Whether the goal of the session on screen is being looked at.
    fn checking_goal(&self) -> bool {
        let on_screen = self.store.conversations[self.store.active].id;
        (self.goal_check.as_ref()).is_some_and(|(id, _)| *id == on_screen)
    }

    /// The session's mode, which opens the list of modes to pick from on a
    /// click, or on Enter or Space while it has the focus.
    fn mode_chip(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let (chat, mode) = (cx.entity(), shown(self.active_mode()));
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
    pub(crate) fn open_mode_menu(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.mode_menu.is_some() {
            return;
        }
        let (chat, mode) = (cx.entity(), shown(self.active_mode()));
        // Closed, the menu hands the keys back to the composer, where /mode
        // was typed; otherwise they would go nowhere.
        let back = self.composer.focus_handle(cx);
        let menu = PopupMenu::build(window, cx, move |menu, _, _| {
            let menu = menu.action_context(back);
            offered().fold(menu, |menu, choice| {
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
        let crew_on = crew::on(self.store.conversations[self.store.active].id);
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
                                images.update(cx, |this, cx| this.pick_pictures(window, cx))
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
            .capture_action(cx.listener(Self::paste_picture))
            // ? in an empty box lists the keys; otherwise it is typed.
            .on_action(cx.listener(|this, _: &KeysOrType, window, cx| {
                if !this.composer_empty {
                    return cx.propagate();
                }
                this.show_keys(window, cx);
            }))
            // Ctrl+Enter sends now what Enter would queue for the next step.
            // With nothing running it stays the new line it always was.
            .on_action(cx.listener(|this, _: &SendNow, window, cx| {
                if !this.busy {
                    return cx.propagate();
                }
                this.send(window, cx);
                this.send_now(cx);
            }))
            .on_action(
                cx.listener(|this, _: &crate::ui::voice::ToggleVoice, window, cx| {
                    this.toggle_voice(window, cx);
                }),
            )
            // Around the input rather than on it, so the list's keys come
            // before the input's own only while the list is up.
            .child(
                self.completion_keys(
                    div()
                        .w_full()
                        .child(Input::new(&self.composer).w_full().appearance(false)),
                    cx,
                ),
            );
        let controls = h_flex()
            .items_center()
            .gap_1()
            // The session's mode sits where the message is written, since it
            // decides what the reply may do without asking.
            .child(self.mode_chip(cx))
            .child(
                Button::new("crew-mode")
                    .plain(cx)
                    .xsmall()
                    .tooltip_with_action(
                        if crew_on {
                            "Crew on in this session: Letronna may run a workflow of \
                             helpers for a big task, each run asking first. Click to stop."
                        } else {
                            "Crew off. Click to let Letronna run workflows of helpers for \
                             big tasks in this session."
                        },
                        &ToggleCrew,
                        None,
                    )
                    .text_color(if crew_on {
                        cx.theme().primary
                    } else {
                        cx.theme().muted_foreground
                    })
                    .child(
                        h_flex()
                            .gap_1()
                            .items_center()
                            .child(Icon::new(AppIcon::Crew).size_3())
                            .child("Crew"),
                    )
                    .on_click(cx.listener(|this, _, _, cx| this.toggle_crew(cx))),
            )
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
            .child(self.mic_button(cx))
            // Stop only where the reply is: in another session it would end
            // a reply that is not on screen.
            // And between turns, while its goal is looked at.
            .when(
                (self.busy && self.busy_elsewhere().is_none()) || self.checking_goal(),
                |row| {
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
                },
            )
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
                    .disabled(
                        (self.composer_empty && self.attached().is_empty())
                            || self.busy_elsewhere().is_some(),
                    )
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
        .py_1()
        .relative()
        .children(
            self.ring(crate::ui::keys::Region::Composer, cx)
                .map(|ring| ring.rounded(cx.theme().radius)),
        );
        // Narrow, the text takes the whole width and the controls go under
        // it, rather than the text being squeezed to a few words a line.
        let frame = if compact {
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
        };
        v_flex()
            .relative()
            .w_full()
            .children(self.attached_row(cx))
            .children(self.accepted_row(cx))
            .child(frame)
            .children(self.completion_list(cx))
    }

    /// Whether the Crew chip shows: only where sending would opt the message
    /// in, since one sent while a reply runs joins it as a follow-up, which
    /// never opts in, and with the switch on it says nothing new.
    fn crew_chip_shown(&self) -> bool {
        let id = self.store.conversations[self.store.active].id;
        self.crew_asked && !self.crew_dismissed && !self.busy && !crew::on(id)
    }

    /// The chip over the composer saying `@crew` opts this message into
    /// Crew, with a way to send it without.
    pub(crate) fn crew_chip(&self, cx: &mut Context<Self>) -> Option<impl IntoElement> {
        if !self.crew_chip_shown() {
            return None;
        }
        let chip = h_flex()
            .gap_1p5()
            .items_center()
            .pl_2()
            .rounded(cx.theme().radius)
            .border_1()
            .border_color(cx.theme().border)
            .bg(cx.theme().secondary.opacity(0.3))
            .text_xs()
            .child(
                Icon::new(AppIcon::Crew)
                    .size_3()
                    .text_color(cx.theme().primary),
            )
            .child("Crew: this message")
            .child(
                Button::new("crew-chip-close")
                    .plain(cx)
                    .xsmall()
                    .icon(Icon::new(IconName::Close))
                    .tooltip_with_action("Send it without Crew", &DismissCrew, None)
                    .on_click(cx.listener(|this, _, window, cx| this.dismiss_crew(window, cx))),
            );
        Some(h_flex().w_full().mb_2().child(chip))
    }

    /// Turns the Crew switch of the open session over.
    pub(crate) fn toggle_crew(&mut self, cx: &mut Context<Self>) {
        turn_crew(self.store.conversations[self.store.active].id);
        cx.notify();
    }

    /// Dismisses the Crew chip, so the message goes without Crew.
    pub(crate) fn dismiss_crew(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.crew_chip_shown() {
            // With no chip the key types what it would, such as "∑" on a Mac.
            cx.propagate();
            return;
        }
        self.crew_dismissed = true;
        self.focus_composer(window, cx);
        cx.notify();
    }

    /// Notes whether the composer's words ask for Crew, redrawing only when
    /// that changes; a chip dismissed for words that no longer ask comes back
    /// when they ask again.
    pub(crate) fn crew_typed(&mut self, cx: &mut Context<Self>) {
        let asked = crew_asked(&self.composer.read(cx).value());
        if asked == self.crew_asked {
            return;
        }
        self.crew_asked = asked;
        self.crew_dismissed = false;
        cx.notify();
    }

    /// Notes afresh whether the composer's words ask for Crew once they were
    /// put there rather than typed, as when a session's draft comes back or
    /// a message leaves, so a chip dismissed for other words stays with them.
    pub(crate) fn crew_reset(&mut self, cx: &mut Context<Self>) {
        self.crew_asked = crew_asked(&self.composer.read(cx).value());
        self.crew_dismissed = false;
        cx.notify();
    }
}

/// Turns the Crew switch of `session` over.
fn turn_crew(session: u64) {
    crew::set(session, !crew::on(session));
}

/// Whether the composer's `text` asks for Crew, by core's rules for words
/// typed into the composer.
fn crew_asked(text: &str) -> bool {
    crew::requested(text, crew::Source::Composer)
}

/// The words `crew::prompt` hears for `prompt`: as typed, or with each
/// `@crew` made a plain "crew" once its chip was `dismissed`, so the message
/// goes without Crew while the model still reads it as typed.
pub(crate) fn crew_words(prompt: &str, dismissed: bool) -> Cow<'_, str> {
    if !dismissed {
        return Cow::Borrowed(prompt);
    }
    // Lowering only ASCII leaves every byte where it was, so a place found
    // in the copy is the same place in `prompt`.
    let lower = prompt.to_ascii_lowercase();
    let mut words = String::with_capacity(prompt.len());
    let mut from = 0;
    for (at, _) in lower.match_indices("@crew") {
        words.push_str(&prompt[from..at]);
        // Past the one-byte "@".
        from = at + 1;
    }
    words.push_str(&prompt[from..]);
    Cow::Owned(words)
}

#[cfg(test)]
mod tests {
    // Not `use super::*`: the prelude re-exports gpui, whose `test` macro
    // would shadow the built-in attribute.
    use super::{crew_asked, crew_words, turn_crew};
    use letronna_core::crew;

    #[test]
    fn the_crew_switch_turns_over_in_its_own_session_only() {
        // Far from the ids of real sessions, since the switches belong to
        // the process.
        let (here, there) = (u64::MAX - 46, u64::MAX - 47);
        turn_crew(here);
        assert!(crew::on(here));
        assert!(!crew::on(there));
        turn_crew(there);
        turn_crew(here);
        assert!(!crew::on(here));
        assert!(crew::on(there));
        turn_crew(there);
        assert!(!crew::on(there));
    }

    #[test]
    fn the_chip_shows_for_at_crew_typed_as_a_word() {
        let meant = [
            "@crew",
            "@CREW find every bug",
            "audit this (@crew)",
            "`code` then @crew",
        ];
        for text in meant {
            assert!(crew_asked(text), "{text:?}");
        }
    }

    #[test]
    fn the_chip_stays_away_for_code_quotes_paths_urls_and_commands() {
        let quoted = [
            "what does `@crew` do?",
            "```\n@crew\n```",
            "the word '@crew' is special",
            "the word \"@crew\" is special",
            "open src/@crew/mod.rs",
            "see https://example.com/@crew",
            "mail team@crew.io",
            "/review @crew",
            "/crew on",
        ];
        for text in quoted {
            assert!(!crew_asked(text), "{text:?}");
        }
    }

    #[test]
    fn a_dismissed_chip_sends_the_message_without_crew() {
        let typed = "über @Crew, then @crew's plan";
        assert!(crew_asked(&crew_words(typed, false)));
        let sent = crew_words(typed, true);
        assert_eq!(sent, "über Crew, then crew's plan");
        assert!(!crew_asked(&sent));
    }
}

/// What a key adds to a message when typed outside any text box: its
/// character, if it has one that shows, and no shortcut modifier.
fn typed(keystroke: &Keystroke) -> Option<&str> {
    let held = keystroke.modifiers;
    if held.control || held.platform || held.function {
        return None;
    }
    // Space and Enter press buttons, which must not type as well.
    keystroke
        .key_char
        .as_deref()
        .filter(|text| text.chars().any(|c| !c.is_whitespace() && !c.is_control()))
}

#[cfg(test)]
mod typed_tests {
    // Not `use super::*`: the prelude re-exports gpui, whose `test` macro
    // would shadow the built-in attribute.
    use super::typed;
    use gpui::Keystroke;

    #[test]
    fn a_character_typed_outside_a_box_is_kept_and_a_shortcut_is_not() {
        let key = |source| Keystroke::parse(source).unwrap_or_default();
        assert_eq!(typed(&key("x->x")), Some("x"));
        assert_eq!(typed(&key("shift-x->X")), Some("X"));
        assert_eq!(typed(&key("ctrl-x->x")), None);
        assert_eq!(typed(&key("f5")), None);
        let space = Keystroke {
            key: "space".into(),
            key_char: Some(" ".into()),
            ..Default::default()
        };
        assert_eq!(typed(&space), None);
    }
}

#[cfg(test)]
mod mode_tests {
    // Not `use super::*`: the prelude re-exports gpui, whose `test` macro
    // would shadow the built-in attribute.
    use super::{offered, shown};
    use letronna_core::gate::Mode;

    #[test]
    fn every_mode_shows_as_one_the_list_offers() {
        for mode in Mode::OFFERED {
            assert!(offered().any(|choice| choice == shown(mode)), "{mode:?}");
        }
        assert_eq!(shown(Mode::Auto), Mode::AcceptEdits);
    }
}
