use crate::prelude::*;
use letronna_core::gate::Mode;

type ApplyMode = std::rc::Rc<dyn Fn(plugins::PresenceMode, &mut App)>;

const PAGES: &[(&str, AppIcon)] = &[
    ("Account", AppIcon::User),
    ("Gateways", AppIcon::Plugs),
    ("Search", AppIcon::Search),
    ("Model", AppIcon::Robot),
    ("Appearance", AppIcon::Palette),
    ("Thinking", AppIcon::Sparkles),
    ("Privacy", AppIcon::Shield),
    ("Plugins", AppIcon::Puzzle),
    ("About", AppIcon::Info),
];

impl Chat {
    /// Open a gateway's row to set it up, or close it if it is already open.
    pub(crate) fn select_gateway(
        &mut self,
        name: &'static str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let open = *self.gateway_pick.read(cx) == name;
        self.gateway_pick.update(cx, |g, cx| {
            *g = if open { String::new() } else { name.into() };
            cx.notify();
        });
        if open {
            return;
        }
        self.endpoint.update(cx, |input, cx| {
            input.set_value(self.store.settings.custom_endpoint.clone(), window, cx)
        });
        let key = self.keys.get(key_slot(name)).cloned().unwrap_or_default();
        self.key
            .update(cx, |input, cx| input.set_value(key, window, cx));
        cx.notify();
    }

    /// Use the browser at the address in the field, or with none this
    /// computer's; the one running now is let go of, its pages kept.
    pub(crate) fn save_browser_address(&mut self, cx: &mut Context<Self>) {
        let address = self.browser_address.read(cx).value().trim().to_string();
        let slot = letronna_core::browser::ELSEWHERE;
        if address.is_empty() {
            self.keys.remove(slot);
        } else {
            self.keys.insert(slot.to_owned(), address);
        }
        self.write_keys();
        letronna_core::browser::restart();
        cx.notify();
    }

    /// Go back to the browser on this computer.
    pub(crate) fn clear_browser_address(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.browser_address
            .update(cx, |input, cx| input.set_value("", window, cx));
        self.save_browser_address(cx);
    }

    fn write_keys(&mut self) {
        if std::fs::create_dir_all(backend::data_dir())
            .and_then(|_| {
                std::fs::write(keys_path(), serde_json::to_vec_pretty(&self.keys).unwrap())
            })
            .is_err()
        {
            self.error = Some("Could not save the API key.".into());
        }
    }

    /// Save the open row's key (and URL, for Custom), close it, and reload
    /// every gateway's models.
    pub(crate) fn save_gateway(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let editing = self.gateway_pick.read(cx).clone();
        if editing.is_empty() {
            return;
        }
        if gateway(&editing).base.is_empty() {
            self.store.settings.custom_endpoint = self.endpoint.read(cx).value().trim().to_string();
        }
        let key = self.key.read(cx).value().trim().to_string();
        let slot = key_slot(&editing).to_string();
        if key.is_empty() {
            self.keys.remove(&slot);
        } else {
            self.keys.insert(slot, key);
        }
        self.write_keys();
        self.save();
        self.gateway_pick.update(cx, |g, cx| {
            g.clear();
            cx.notify();
        });
        self.load_models(window, cx);
    }

    /// Forget a gateway's key (and URL, for Custom); its models leave the list.
    pub(crate) fn remove_gateway(
        &mut self,
        name: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.keys.remove(key_slot(name));
        if gateway(name).base.is_empty() {
            self.store.settings.custom_endpoint.clear();
        }
        self.write_keys();
        self.save();
        self.gateway_pick.update(cx, |g, cx| {
            g.clear();
            cx.notify();
        });
        self.load_models(window, cx);
    }

    /// Open a search service's row to set it up, or close it if it is open.
    /// It shares the gateways' fields: one row is open at a time.
    pub(crate) fn select_search(
        &mut self,
        name: &'static str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let row = format!("search:{name}");
        let open = *self.gateway_pick.read(cx) == row;
        self.gateway_pick.update(cx, |g, cx| {
            *g = if open { String::new() } else { row };
            cx.notify();
        });
        let Some(service) = letronna_core::search::service(name).filter(|_| !open) else {
            return;
        };
        let address = letronna_core::search::choice().address(service);
        self.endpoint
            .update(cx, |input, cx| input.set_value(address, window, cx));
        let key = self.keys.get(name).cloned().unwrap_or_default();
        self.key
            .update(cx, |input, cx| input.set_value(key, window, cx));
        cx.notify();
    }

    /// Put the open search row's address back to the service's official one.
    pub(crate) fn reset_search_address(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let open = self.gateway_pick.read(cx).clone();
        let Some(service) = open
            .strip_prefix("search:")
            .and_then(letronna_core::search::service)
        else {
            return;
        };
        self.endpoint.update(cx, |input, cx| {
            input.set_value(service.official, window, cx)
        });
    }

    /// Save the open search row's key and address, and search with it from now on.
    pub(crate) fn save_search(&mut self, cx: &mut Context<Self>) {
        let open = self.gateway_pick.read(cx).clone();
        let Some(service) = open
            .strip_prefix("search:")
            .and_then(letronna_core::search::service)
        else {
            return;
        };
        let mut choice = letronna_core::search::choice();
        choice.set_address(service, self.endpoint.read(cx).value().as_ref());
        if !service.self_hosted() {
            let key = self.key.read(cx).value().trim().to_string();
            if key.is_empty() {
                self.keys.remove(service.name);
            } else {
                self.keys.insert(service.name.to_owned(), key);
            }
            self.write_keys();
        }
        choice.service = service.name.to_owned();
        if let Err(error) = letronna_core::search::choose(&choice) {
            self.error = Some(error);
        }
        self.gateway_pick.update(cx, |g, cx| {
            g.clear();
            cx.notify();
        });
        cx.notify();
    }

    /// Forget a search service's key and address; if it was the one in use,
    /// the agent no longer searches.
    pub(crate) fn remove_search(&mut self, name: &str, cx: &mut Context<Self>) {
        let Some(service) = letronna_core::search::service(name) else {
            return;
        };
        let mut choice = letronna_core::search::choice();
        choice.set_address(service, "");
        if self.keys.remove(name).is_some() {
            self.write_keys();
        }
        if choice.service == name {
            choice.service.clear();
        }
        if let Err(error) = letronna_core::search::choose(&choice) {
            self.error = Some(error);
        }
        self.gateway_pick.update(cx, |g, cx| {
            g.clear();
            cx.notify();
        });
        cx.notify();
    }

    /// The Model page's Save: the default model.
    pub(crate) fn apply_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.store.settings.model = self.model.read(cx).value().to_string();
        // Cleared before saving, not after: clearing last threw away the very
        // error that says the save failed.
        self.error = None;
        self.save();
        self.load_models(window, cx);
        self.close_settings(window, cx);
    }

    pub(crate) fn open_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // Snapshot the window before the overlay covers it. Platforms that
        // cannot sample it return None, which falls back to the plain scrim.
        self.settings_backdrop = crate::platform::backdrop::capture(window);
        self.settings_open = true;
        self.gateway_pick.update(cx, |g, _| g.clear());
        self.settings_closing = false;
        // Somewhere inside the chat has to hold focus for escape to reach
        // it; opened from a menu, nothing does.
        self.settings_focus.focus(window);
        // The terminal may have signed in or out since this last looked.
        self.refresh_account(cx);
        cx.notify();
    }

    pub(crate) fn close_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.settings_closing {
            return;
        }
        self.settings_closing = true;
        self.focus_composer(window, cx);
        cx.notify();
        cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(std::time::Duration::from_millis(170))
                .await;
            let _ = this.update(cx, |this, cx| {
                this.settings_open = false;
                this.settings_closing = false;
                this.settings_backdrop = None;
                cx.notify();
            });
        })
        .detach();
    }

    /// The settings overlay, mounted while open so both open and close animate.
    pub(crate) fn settings_overlay(
        &self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        if !self.settings_open {
            return None;
        }
        let closing = self.settings_closing;
        let anim = || {
            Animation::new(std::time::Duration::from_millis(180))
                .with_easing(gpui::ease_out_quint())
        };
        let blur = self.settings_backdrop.clone().map(|image| {
            img(image)
                .absolute()
                .size_full()
                .object_fit(ObjectFit::Fill)
                .with_animation(
                    if closing { "blur-out" } else { "blur-in" },
                    anim(),
                    move |el, delta| el.opacity(if closing { 1. - delta } else { delta }),
                )
        });
        let scrim = div()
            .id("settings-scrim")
            .absolute()
            .size_full()
            .bg(gpui::black())
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, window, cx| this.close_settings(window, cx)),
            )
            .with_animation(
                if closing { "scrim-out" } else { "scrim-in" },
                anim(),
                move |el, delta| {
                    el.opacity(if closing {
                        0.12 * (1. - delta)
                    } else {
                        0.12 * delta
                    })
                },
            );
        let panel = self
            .settings_panel(window, cx)
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .with_animation(
                if closing { "panel-out" } else { "panel-in" },
                anim(),
                move |el, delta| {
                    let p = if closing { 1. - delta } else { delta };
                    el.opacity(p).mt(px(-14. * (1. - p)))
                },
            );
        Some(
            div()
                .absolute()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .children(blur)
                .child(scrim)
                .child(panel)
                .into_any_element(),
        )
    }

    fn settings_panel(&self, window: &mut Window, cx: &mut Context<Self>) -> Div {
        let endpoint = self.endpoint.clone();
        let model = self.model.clone();
        let key = self.key.clone();
        let chat = cx.entity();
        let pick = self.gateway_pick.clone();
        let presence = self.presence_pick.clone();
        let page = self.settings_page.clone();
        let vp = window.viewport_size();
        let gap = px(44.);
        let height = vp.height - gap * 2.;
        let current_page = *page.read(cx);
        let nav = PAGES.iter().fold(
            v_flex()
                .w(px(210.))
                .h_full()
                .flex_shrink_0()
                .gap_0p5()
                .p_3()
                .bg(cx.theme().sidebar)
                .border_r_1()
                .border_color(cx.theme().border),
            |nav, (name, icon)| {
                let page = page.clone();
                let active = *name == current_page;
                nav.child(
                    h_flex()
                        .id(*name)
                        .h(px(30.))
                        .px_2()
                        .gap_2()
                        .items_center()
                        .rounded(cx.theme().radius)
                        .cursor_pointer()
                        .text_sm()
                        .font_family(HEADING_FONT)
                        .font_weight(FontWeight::MEDIUM)
                        .when(active, |this| this.bg(cx.theme().sidebar_accent))
                        .hover(|s| s.bg(cx.theme().sidebar_accent.opacity(0.6)))
                        .child(
                            Icon::new(*icon)
                                .size_4()
                                .text_color(cx.theme().muted_foreground),
                        )
                        .child(*name)
                        .on_click(move |_, _, cx| {
                            page.update(cx, |p, cx| {
                                *p = name;
                                cx.notify();
                            })
                        }),
                )
            },
        );
        let body: AnyElement = match current_page {
            "Model" => model_page(&model, &chat, cx).into_any_element(),
            "Appearance" => text_page(
                "Appearance",
                "Dark theme with the Letronna palette. Theme and font options land here later.",
                cx,
            )
            .into_any_element(),
            "Privacy" => privacy_page(
                &chat,
                &presence,
                &self.browser_address,
                &self.store.settings,
                cx,
            )
            .into_any_element(),
            "Account" => super::account::account_page(&chat, self, cx).into_any_element(),
            "Thinking" => thinking_page(&chat, &self.store.settings, cx).into_any_element(),
            "Plugins" => plugins_page(&chat, cx).into_any_element(),
            "About" => about_page(&chat, self, cx).into_any_element(),
            "Search" => search_page(self.search_rows(), &pick, &endpoint, &key, &chat, cx)
                .into_any_element(),
            _ => gateway_page(
                self.gateway_rows(),
                &pick,
                &endpoint,
                &key,
                &page,
                &chat,
                cx,
            )
            .into_any_element(),
        };
        div()
            .track_focus(&self.settings_focus)
            .w(vp.width - gap * 2.)
            .bg(cx.theme().background)
            .border_1()
            .border_color(cx.theme().border)
            .rounded(cx.theme().radius_lg)
            .shadow_lg()
            .child(
                h_flex()
                    .h(height)
                    .items_start()
                    .overflow_hidden()
                    .rounded(cx.theme().radius_lg)
                    .child(nav)
                    .child(
                        div()
                            .relative()
                            .flex_1()
                            .min_w_0()
                            .h_full()
                            .child(
                                div()
                                    .id("settings-body")
                                    .size_full()
                                    .overflow_y_scroll()
                                    .px_8()
                                    .py_6()
                                    .child(
                                        div().child(body).with_animation(
                                            SharedString::from(format!("page-{current_page}")),
                                            Animation::new(std::time::Duration::from_millis(200))
                                                .with_easing(gpui::ease_out_quint()),
                                            |el, delta| el.opacity(delta).mt(px(6. * (1. - delta))),
                                        ),
                                    ),
                            )
                            .child(
                                div().absolute().top_3().right_3().child(
                                    Button::new("close-settings")
                                        .ghost()
                                        .xsmall()
                                        .icon(Icon::new(IconName::Close))
                                        .on_click(cx.listener(|this, _, window, cx| {
                                            this.close_settings(window, cx)
                                        })),
                                ),
                            ),
                    ),
            )
    }
}

fn plugins_page(chat: &Entity<Chat>, cx: &App) -> Div {
    let mut list = v_flex().gap_2();
    for info in plugins::list(cx) {
        let chat = chat.clone();
        let id = info.id;
        let enabled = info.enabled;
        let row = h_flex()
            .items_center()
            .justify_between()
            .gap_3()
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .gap_0p5()
                    .child(
                        h_flex()
                            .items_center()
                            .gap_2()
                            .child(
                                div()
                                    .text_sm()
                                    .font_weight(FontWeight::MEDIUM)
                                    .child(info.name),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .px_1p5()
                                    .rounded(cx.theme().radius)
                                    .bg(cx.theme().secondary)
                                    .text_color(cx.theme().muted_foreground)
                                    .child(if info.builtin { "Built in" } else { "External" }),
                            ),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(info.description),
                    ),
            )
            .child(
                div().flex_shrink_0().cursor_pointer().child(
                    gpui_component::switch::Switch::new(SharedString::from(format!("plugin-{id}")))
                        .checked(enabled)
                        .on_click(move |_, _, cx| {
                            plugins::set_enabled(cx, id, !enabled);
                            chat.update(cx, |_, cx| cx.notify());
                        }),
                ),
            );
        let settings = if enabled {
            plugins::render_settings(cx, id)
        } else {
            None
        };
        list = list.child(
            v_flex()
                .gap_2()
                .p_3()
                .rounded(cx.theme().radius)
                .bg(cx.theme().secondary.opacity(0.3))
                .child(row)
                .children(settings),
        );
    }
    v_flex()
        .gap_4()
        .child(heading(
            "Plugins",
            "Optional integrations. Toggle one off to unload it. Choices are saved in plugins.json.",
            cx,
        ))
        .child(list)
}

pub(crate) fn heading(title: &'static str, blurb: &'static str, cx: &App) -> Div {
    v_flex()
        .gap_1()
        .child(
            div()
                .text_base()
                .font_family(HEADING_FONT)
                .font_weight(FontWeight::SEMIBOLD)
                .child(title),
        )
        .child(
            div()
                .text_sm()
                .text_color(cx.theme().muted_foreground)
                .child(blurb),
        )
}

pub(crate) fn label(text: &'static str, cx: &App) -> Div {
    div()
        .text_xs()
        .font_family(HEADING_FONT)
        .text_color(cx.theme().muted_foreground)
        .child(text)
}

/// Where the browser runs: this computer, or an address given for one
/// elsewhere.
fn elsewhere_row(chat: &Entity<Chat>, address: &Entity<InputState>, cx: &App) -> Div {
    let now = match letronna_core::browser::remote_place() {
        Some(place) => format!("Now the browser at {place}."),
        None => "Now Chrome or Edge on this computer.".to_owned(),
    };
    let (save, clear) = (chat.clone(), chat.clone());
    v_flex()
        .gap_2()
        .px_3()
        .py_2p5()
        .rounded(cx.theme().radius)
        .bg(cx.theme().secondary.opacity(0.3))
        .child(
            v_flex()
                .gap_0p5()
                .child(div().text_sm().child("Browser elsewhere"))
                .child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(format!(
                            "A cloud browser's connect address, its key in it as the service \
                             gives it (wss://…?token=…), or Chrome on another machine started \
                             with --remote-debugging-port (http://its-address:9222). Empty, the \
                             browser runs on this computer. {now}"
                        )),
                ),
        )
        .child(
            h_flex()
                .gap_2()
                .child(div().flex_1().child(Input::new(address)))
                .child(
                    Button::new("browser-elsewhere-clear")
                        .ghost()
                        .small()
                        .label("This computer")
                        .on_click(move |_, window, cx| {
                            clear.update(cx, |this, cx| this.clear_browser_address(window, cx))
                        }),
                )
                .child(
                    Button::new("browser-elsewhere-save")
                        .primary()
                        .small()
                        .label("Save")
                        .on_click(move |_, _, cx| {
                            save.update(cx, |this, cx| this.save_browser_address(cx))
                        }),
                ),
        )
}

fn privacy_page(
    chat: &Entity<Chat>,
    presence: &Entity<String>,
    address: &Entity<InputState>,
    settings: &Settings,
    cx: &App,
) -> Div {
    let (default_mode, shared) = (settings.mode, settings.browser_shared);
    let unseen = settings.browse_unseen;
    let current = plugins::PresenceMode::from_str(presence.read(cx));
    let enabled = plugins::list(cx)
        .iter()
        .find(|p| p.id == plugins::DISCORD)
        .map(|p| p.enabled)
        .unwrap_or(true);

    let apply: ApplyMode = {
        let chat = chat.clone();
        let presence = presence.clone();
        std::rc::Rc::new(move |mode: plugins::PresenceMode, cx: &mut App| {
            presence.update(cx, |p, cx| {
                *p = mode.as_str().into();
                cx.notify();
            });
            plugins::emit(cx, plugins::AppEvent::PresenceMode(mode));
            chat.update(cx, |this, cx| {
                this.store.settings.presence = mode.as_str().into();
                this.save();
                cx.notify();
            });
        })
    };

    let level_label = if current == plugins::PresenceMode::Minimal {
        "Hide activity details"
    } else {
        "Show what I am doing"
    };
    let detailed = apply.clone();
    let minimal = apply.clone();
    let level = h_flex()
        .items_center()
        .justify_between()
        .gap_3()
        .px_3()
        .py_2p5()
        .rounded(cx.theme().radius)
        .bg(cx.theme().secondary.opacity(0.3))
        .child(
            div()
                .text_sm()
                .text_color(cx.theme().muted_foreground)
                .child("What to share"),
        )
        .child(
            Button::new("presence-level")
                .ghost()
                .small()
                .child(
                    h_flex()
                        .gap_1()
                        .items_center()
                        .child(level_label)
                        .child(Icon::new(IconName::ChevronDown).size_3()),
                )
                .dropdown_menu(move |menu, _, _| {
                    let detailed = detailed.clone();
                    let minimal = minimal.clone();
                    menu.min_w(px(220.))
                        .item(
                            PopupMenuItem::new("Show what I am doing")
                                .checked(current == plugins::PresenceMode::Detailed)
                                .on_click(move |_, _, cx| {
                                    detailed(plugins::PresenceMode::Detailed, cx)
                                }),
                        )
                        .item(
                            PopupMenuItem::new("Hide activity details")
                                .checked(current == plugins::PresenceMode::Minimal)
                                .on_click(move |_, _, cx| {
                                    minimal(plugins::PresenceMode::Minimal, cx)
                                }),
                        )
                }),
        );

    let approvals = h_flex()
        .items_center()
        .justify_between()
        .gap_3()
        .px_3()
        .py_2p5()
        .rounded(cx.theme().radius)
        .bg(cx.theme().secondary.opacity(0.3))
        .child(
            v_flex()
                .flex_1()
                .min_w_0()
                .gap_0p5()
                .child(div().text_sm().child("Default mode"))
                .child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(
                            "The mode a session starts in; its chip by the message box changes \
                             it for that session. Bypass writes files and runs commands without \
                             asking, but some commands that are hard to undo, such as recursive \
                             deletes and force pushes, still ask.",
                        ),
                ),
        )
        .child(
            Button::new("default-mode")
                .ghost()
                .small()
                .child(
                    h_flex()
                        .gap_1()
                        .items_center()
                        .child(default_mode.label())
                        .child(Icon::new(IconName::ChevronDown).size_3()),
                )
                .dropdown_menu({
                    let chat = chat.clone();
                    move |menu, _, _| {
                        Mode::OFFERED.into_iter().fold(menu, |menu, choice| {
                            let chat = chat.clone();
                            menu.item(
                                PopupMenuItem::new(choice.label())
                                    .checked(choice == default_mode)
                                    .on_click(move |_, _, cx| {
                                        chat.update(cx, |this, cx| {
                                            this.store.settings.mode = choice;
                                            this.save();
                                            this.approvals_changed(cx);
                                            cx.notify();
                                        })
                                    }),
                            )
                        })
                    }
                }),
        );

    let browser = h_flex()
        .items_center()
        .justify_between()
        .gap_3()
        .px_3()
        .py_2p5()
        .rounded(cx.theme().radius)
        .bg(cx.theme().secondary.opacity(0.3))
        .child(
            v_flex()
                .flex_1()
                .min_w_0()
                .gap_0p5()
                .child(div().text_sm().child("One browser for every session"))
                .child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(
                            "Share the sign-ins and pages of one browser, remembered between \
                             runs. Off, each session gets a private one, forgotten when Letronna \
                             quits.",
                        ),
                ),
        )
        .child(
            div().flex_shrink_0().cursor_pointer().child(
                gpui_component::switch::Switch::new("browser-shared")
                    .checked(shared)
                    .on_click({
                        let chat = chat.clone();
                        move |_, _, cx| {
                            chat.update(cx, |this, cx| {
                                this.store.settings.browser_shared = !shared;
                                this.save();
                                letronna_core::browser::share_browser(!shared);
                                cx.notify();
                            })
                        }
                    }),
            ),
        );

    let seen = h_flex()
        .items_center()
        .justify_between()
        .gap_3()
        .px_3()
        .py_2p5()
        .rounded(cx.theme().radius)
        .bg(cx.theme().secondary.opacity(0.3))
        .child(
            v_flex()
                .flex_1()
                .min_w_0()
                .gap_0p5()
                .child(div().text_sm().child("Show the page as the agent browses"))
                .child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(
                            "The browser opens beside the chat when the agent uses it. Off, the \
                             agent browses unseen, lighter on memory; open the browser from the \
                             menu to look.",
                        ),
                ),
        )
        .child(
            div().flex_shrink_0().cursor_pointer().child(
                gpui_component::switch::Switch::new("browser-seen")
                    .checked(!unseen)
                    .on_click({
                        let chat = chat.clone();
                        move |_, _, cx| {
                            chat.update(cx, |this, cx| {
                                this.store.settings.browse_unseen = !unseen;
                                this.save();
                                cx.notify();
                            })
                        }
                    }),
            ),
        );

    v_flex()
        .gap_4()
        .child(heading(
            "Agent",
            "Write and command tools ask before they act. Turn that off only for folders you trust.",
            cx,
        ))
        .child(approvals)
        .child(heading(
            "Browser",
            "Each session browses in a browser of its own, as a separate agent would.",
            cx,
        ))
        .child(browser)
        .child(seen)
        .child(elsewhere_row(chat, address, cx))
        .child(heading(
            "Discord Rich Presence",
            "What Letronna shares with Discord. The integration is turned on or off in the Plugins tab.",
            cx,
        ))
        .child(if enabled {
            level.into_any_element()
        } else {
            div()
                .p_3()
                .rounded(cx.theme().radius)
                .bg(cx.theme().secondary.opacity(0.3))
                .text_sm()
                .text_color(cx.theme().muted_foreground)
                .child("Discord Rich Presence is off. Enable it in the Plugins tab to choose what to share.")
                .into_any_element()
        })
}

fn about_page(chat: &Entity<Chat>, this: &Chat, cx: &App) -> Div {
    let muted = cx.theme().muted_foreground;
    let line =
        |text: &'static str, size: f32| div().text_size(px(size)).text_color(muted).child(text);
    v_flex()
        .size_full()
        .items_center()
        .justify_center()
        .gap_2()
        .text_center()
        .child(
            img("images/letronna.png")
                .size(px(150.))
                .rounded(cx.theme().radius_lg)
                .mb_3(),
        )
        .child(
            div()
                .font_family(WORDMARK_FONT)
                .italic()
                .text_size(px(36.))
                .child(BRAND),
        )
        .child(line(
            concat!(
                "Version ",
                env!("CARGO_PKG_VERSION"),
                "  ·  build ",
                env!("GIT_HASH")
            ),
            13.,
        ))
        .child(updates(chat, this, cx))
        .child(
            div()
                .text_sm()
                .pt_3()
                .child("A small native desktop agent. Rust, GPU rendered, no browser inside."),
        )
        .child(line("Developed by the Gadlus Engineering team.", 13.))
        .child(
            v_flex()
                .pt_6()
                .gap_1()
                .items_center()
                .child(line(
                    "© 2026 Gadlus Engineering. Released under the MIT License.",
                    12.,
                ))
                .child(line(
                    "Built with GPUI and gpui-component (Apache-2.0). Icons by Phosphor (MIT).",
                    12.,
                )),
        )
}

/// Where updating stands, the button to look now, and whether to look alone.
fn updates(chat: &Entity<Chat>, this: &Chat, cx: &App) -> Div {
    use crate::ui::update::Updating;
    let looking = matches!(this.updating, Updating::Checking | Updating::Downloading(_));
    let (check, next) = (chat.clone(), chat.clone());
    let manual = this.store.settings.manual_updates;
    v_flex()
        .w(px(420.))
        .pt_3()
        .gap_3()
        .items_center()
        .child(
            div()
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .child(this.update_status()),
        )
        .child(
            h_flex()
                .gap_2()
                .child(
                    Button::new("check-updates")
                        .small()
                        .label("Check for updates")
                        .disabled(looking)
                        .on_click(move |_, _, cx| {
                            check.update(cx, |this, cx| this.check_for_updates(true, cx))
                        }),
                )
                .when(
                    matches!(this.updating, Updating::Ready(_) | Updating::Out(_)),
                    |row| {
                        let out = matches!(this.updating, Updating::Out(_));
                        row.child(
                            Button::new("apply-update")
                                .primary()
                                .small()
                                .label(if out { "Download" } else { "Restart now" })
                                .disabled(this.busy)
                                .on_click(move |_, _, cx| {
                                    next.update(cx, |this, cx| {
                                        if out {
                                            this.download_update(cx)
                                        } else {
                                            this.restart_to_update(cx)
                                        }
                                    })
                                }),
                        )
                    },
                ),
        )
        .child(div().w_full().text_left().child(toggle_row(
            "auto-update",
            "Update automatically",
            "Download new versions in the background; they install when Letronna restarts.",
            !manual,
            chat,
            cx,
            std::rc::Rc::new(move |this: &mut Chat| this.store.settings.manual_updates = !manual),
        )))
}

fn text_page(title: &'static str, blurb: &'static str, cx: &App) -> Div {
    v_flex().gap_4().child(heading(title, blurb, cx))
}

fn model_page(model: &Entity<InputState>, chat: &Entity<Chat>, cx: &App) -> Div {
    let save = chat.clone();
    v_flex()
        .gap_4()
        .child(heading(
            "Model",
            "The model used for new messages. Pick from the composer, or type an id here.",
            cx,
        ))
        .child(
            v_flex()
                .gap_1()
                .child(label("Default model", cx))
                .child(Input::new(model)),
        )
        .child(
            h_flex().justify_end().child(
                Button::new("save-model")
                    .primary()
                    .small()
                    .label("Save")
                    .on_click(move |_, window, cx| {
                        save.update(cx, |this, cx| this.apply_settings(window, cx))
                    }),
            ),
        )
}

/// How hard the model thinks, whether that thinking is shown, and whether it
/// may look things up.
///
/// A model that cannot be told how hard to think ignores the setting, so
/// there is nothing here that has to be matched against a capability list --
/// and no list to match against, since the gateway does not publish one.
fn thinking_page(chat: &Entity<Chat>, settings: &Settings, cx: &App) -> Div {
    const LEVELS: [(&str, &str); 4] = [
        ("", "Whatever the model likes"),
        ("low", "Low — quick answers"),
        ("medium", "Medium"),
        ("high", "High — slower, and it shows"),
    ];

    let mut levels = v_flex().gap_2();
    for (value, text) in LEVELS {
        let active = settings.effort == value;
        let chat = chat.clone();
        levels = levels.child(
            h_flex()
                .id(if value.is_empty() { "auto" } else { value })
                .cursor_pointer()
                .items_center()
                .gap_2p5()
                .p_2p5()
                .rounded(cx.theme().radius)
                .border_1()
                .border_color(if active {
                    cx.theme().primary
                } else {
                    cx.theme().border
                })
                .child(div().text_sm().child(text))
                .on_click(move |_, _, cx| {
                    chat.update(cx, |this, cx| {
                        this.store.settings.effort = value.to_string();
                        this.save();
                        cx.notify();
                    })
                }),
        );
    }

    let showing = settings.thinking;
    let show = toggle_row(
        "show-thinking",
        "Show the thinking",
        "Print the model's reasoning as it arrives, not only the answer it settles on.",
        showing,
        chat,
        cx,
        std::rc::Rc::new(move |this: &mut Chat| this.store.settings.thinking = !showing),
    );

    let searching = settings.search;
    let search = toggle_row(
        "web-search",
        "Search the web",
        "Let the gateway look things up for an answer, and cite the pages it read.",
        searching,
        chat,
        cx,
        std::rc::Rc::new(move |this: &mut Chat| this.store.settings.search = !searching),
    );

    v_flex()
        .gap_4()
        .child(heading(
            "Thinking",
            "Models that can be told how hard to think will; the rest carry on as they are.",
            cx,
        ))
        .child(v_flex().gap_1().child(label("Effort", cx)).child(levels))
        .child(show)
        .child(search)
}

/// A switch with a name and a line about what it does.
fn toggle_row(
    id: &'static str,
    title: &'static str,
    blurb: &'static str,
    on: bool,
    chat: &Entity<Chat>,
    cx: &App,
    set: std::rc::Rc<dyn Fn(&mut Chat)>,
) -> Div {
    let chat = chat.clone();
    h_flex()
        .justify_between()
        .items_center()
        .gap_4()
        .child(
            v_flex()
                .gap_0p5()
                .child(div().text_sm().child(title))
                .child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(blurb),
                ),
        )
        .child(
            div().cursor_pointer().child(
                gpui_component::switch::Switch::new(id)
                    .checked(on)
                    .on_click(move |_, _, cx| {
                        let set = set.clone();
                        chat.update(cx, |this, cx| {
                            set(this);
                            this.save();
                            cx.notify();
                        })
                    }),
            ),
        )
}

/// One gateway as the settings list shows it.
pub(crate) struct GatewayRow {
    name: &'static str,
    blurb: &'static str,
    status: String,
    good: bool,
    /// The row's button: "Add key", "Add URL", "Edit", "Sign in", or none.
    action: Option<&'static str>,
    set_up: bool,
}

impl Chat {
    /// Every gateway with where it stands now, taken before the dialog is
    /// built so the dialog never has to read this entity.
    pub(crate) fn gateway_rows(&self) -> Vec<GatewayRow> {
        GATEWAYS
            .iter()
            .map(|g| {
                let count = self.models.iter().filter(|(n, _)| n == g.name).count();
                let (status, good, set_up) = status(
                    g.name,
                    &self.keys,
                    &self.store.settings.custom_endpoint,
                    count,
                );
                let action = match g.name {
                    LETRONNA if !set_up => Some("Sign in"),
                    LETRONNA | "Ollama" => None,
                    "Custom" if !set_up => Some("Add URL"),
                    _ if !set_up => Some("Add key"),
                    _ => Some("Edit"),
                };
                let status = if self.loading_models && set_up {
                    "Checking…".into()
                } else {
                    status
                };
                GatewayRow {
                    name: g.name,
                    blurb: g.blurb,
                    status,
                    good,
                    action,
                    set_up,
                }
            })
            .collect()
    }
}

/// One search service as the settings list shows it.
pub(crate) struct SearchRow {
    name: &'static str,
    blurb: &'static str,
    status: &'static str,
    good: bool,
    set_up: bool,
    self_hosted: bool,
}

impl Chat {
    /// Every search service with where it stands now.
    pub(crate) fn search_rows(&self) -> Vec<SearchRow> {
        let choice = letronna_core::search::choice();
        letronna_core::search::SERVICES
            .iter()
            .map(|s| {
                let (status, good, set_up) =
                    letronna_core::search::status(s.name, &choice, &self.keys);
                SearchRow {
                    name: s.name,
                    blurb: s.blurb,
                    status,
                    good,
                    set_up,
                    self_hosted: s.self_hosted(),
                }
            })
            .collect()
    }
}

fn search_page(
    rows: Vec<SearchRow>,
    pick: &Entity<String>,
    endpoint: &Entity<InputState>,
    key: &Entity<InputState>,
    chat: &Entity<Chat>,
    cx: &App,
) -> Div {
    let open = pick.read(cx).clone();
    let mut list = v_flex()
        .border_1()
        .border_color(cx.theme().border)
        .rounded(cx.theme().radius);
    for (i, row) in rows.into_iter().enumerate() {
        let name = row.name;
        let editing = open.strip_prefix("search:") == Some(name);
        let dot = if row.good {
            cx.theme().green
        } else {
            cx.theme().muted_foreground.opacity(0.5)
        };
        let action = match (row.set_up, row.self_hosted) {
            (true, _) => "Edit",
            (false, true) => "Add address",
            (false, false) => "Add key",
        };
        let button = {
            let chat = chat.clone();
            Button::new(("search-action", i))
                .small()
                .when(!row.set_up, |b| b.primary())
                .when(row.set_up, |b| b.ghost())
                .label(if editing { "Close" } else { action })
                .on_click(move |_, window, cx| {
                    chat.update(cx, |this, cx| this.select_search(name, window, cx))
                })
        };
        let head = h_flex()
            .px_4()
            .py_3()
            .gap_4()
            .items_center()
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .gap_0p5()
                    .child(
                        div()
                            .text_sm()
                            .font_family(HEADING_FONT)
                            .font_weight(FontWeight::MEDIUM)
                            .child(name),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(row.blurb),
                    ),
            )
            .child(
                h_flex()
                    .gap_2()
                    .items_center()
                    .flex_shrink_0()
                    .child(div().size(px(7.)).rounded_full().bg(dot))
                    .child(
                        div()
                            .text_xs()
                            .text_color(if row.good {
                                cx.theme().foreground
                            } else {
                                cx.theme().muted_foreground
                            })
                            .child(row.status),
                    ),
            )
            .child(div().w(px(96.)).flex().justify_end().child(button));
        let mut item = v_flex()
            .when(i > 0, |d| d.border_t_1().border_color(cx.theme().border))
            .child(head);
        if editing {
            let (save, remove, reset) = (chat.clone(), chat.clone(), chat.clone());
            // A service's address starts as its official one; SearXNG has
            // none to go back to.
            let address = match row.self_hosted {
                true => v_flex()
                    .gap_1()
                    .child(label("Address, like http://mac-mini:8888", cx))
                    .child(Input::new(endpoint)),
                false => v_flex().gap_1().child(label("Address", cx)).child(
                    h_flex()
                        .gap_2()
                        .child(div().flex_1().child(Input::new(endpoint)))
                        .child(
                            Button::new("search-reset")
                                .ghost()
                                .small()
                                .label("Reset")
                                .on_click(move |_, window, cx| {
                                    reset.update(cx, |this, cx| {
                                        this.reset_search_address(window, cx)
                                    })
                                }),
                        ),
                ),
            };
            item = item.child(
                v_flex()
                    .px_4()
                    .pb_4()
                    .gap_3()
                    .when(!row.self_hosted, |d| {
                        d.child(
                            v_flex()
                                .gap_1()
                                .child(label("API key", cx))
                                .child(Input::new(key)),
                        )
                    })
                    .child(address)
                    .child(
                        h_flex()
                            .gap_2()
                            .justify_end()
                            .when(row.set_up, |d| {
                                d.child(
                                    Button::new("search-remove")
                                        .ghost()
                                        .small()
                                        .label("Remove")
                                        .on_click(move |_, _, cx| {
                                            remove
                                                .update(cx, |this, cx| this.remove_search(name, cx))
                                        }),
                                )
                            })
                            .child(
                                Button::new("search-save")
                                    .primary()
                                    .small()
                                    .label("Save and use")
                                    .on_click(move |_, _, cx| {
                                        save.update(cx, |this, cx| this.save_search(cx))
                                    }),
                            ),
                    ),
            );
        }
        list = list.child(item);
    }
    v_flex()
        .gap_5()
        .child(heading(
            "Search",
            "A search service of your own, which the agent searches the web with, asking you before each search. The one saved last is used, at its official address unless you give another. Keys stay in keys.json on this machine; the Web toggle is the gateway's own search, where it has one.",
            cx,
        ))
        .child(list)
}

fn gateway_page(
    rows: Vec<GatewayRow>,
    pick: &Entity<String>,
    endpoint: &Entity<InputState>,
    key: &Entity<InputState>,
    page: &Entity<&'static str>,
    chat: &Entity<Chat>,
    cx: &App,
) -> Div {
    let open = pick.read(cx).clone();
    let mut list = v_flex()
        .border_1()
        .border_color(cx.theme().border)
        .rounded(cx.theme().radius);
    for (i, row) in rows.into_iter().enumerate() {
        let name = row.name;
        let editing = open == name;
        let dot = if row.good {
            cx.theme().green
        } else {
            cx.theme().muted_foreground.opacity(0.5)
        };
        let button = row.action.map(|action| {
            let chat = chat.clone();
            let page = page.clone();
            Button::new(("gw-action", i))
                .small()
                .when(!row.set_up, |b| b.primary())
                .when(row.set_up, |b| b.ghost())
                .label(if editing { "Close" } else { action })
                .on_click(move |_, window, cx| {
                    if action == "Sign in" {
                        page.update(cx, |p, cx| {
                            *p = "Account";
                            cx.notify();
                        });
                    } else {
                        chat.update(cx, |this, cx| this.select_gateway(name, window, cx));
                    }
                })
        });
        let head = h_flex()
            .px_4()
            .py_3()
            .gap_4()
            .items_center()
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .gap_0p5()
                    .child(
                        div()
                            .text_sm()
                            .font_family(HEADING_FONT)
                            .font_weight(FontWeight::MEDIUM)
                            .child(name),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(row.blurb),
                    ),
            )
            .child(
                h_flex()
                    .gap_2()
                    .items_center()
                    .flex_shrink_0()
                    .child(div().size(px(7.)).rounded_full().bg(dot))
                    .child(
                        div()
                            .text_xs()
                            .text_color(if row.good {
                                cx.theme().foreground
                            } else {
                                cx.theme().muted_foreground
                            })
                            .child(row.status),
                    ),
            )
            .child(div().w(px(84.)).flex().justify_end().children(button));
        let mut item = v_flex()
            .when(i > 0, |d| d.border_t_1().border_color(cx.theme().border))
            .child(head);
        if editing {
            let custom = gateway(name).base.is_empty();
            let needs_key = gateway(name).needs_key;
            let save = chat.clone();
            let remove = chat.clone();
            item = item.child(
                v_flex()
                    .px_4()
                    .pb_4()
                    .gap_3()
                    .when(custom, |d| {
                        d.child(
                            v_flex()
                                .gap_1()
                                .child(label("Base URL", cx))
                                .child(Input::new(endpoint)),
                        )
                    })
                    .child(
                        v_flex()
                            .gap_1()
                            .child(label(
                                if needs_key {
                                    "API key"
                                } else {
                                    "API key (optional)"
                                },
                                cx,
                            ))
                            .child(Input::new(key).mask_toggle()),
                    )
                    .child(
                        h_flex()
                            .gap_2()
                            .justify_end()
                            .when(row.set_up, |d| {
                                d.child(
                                    Button::new("gw-remove")
                                        .ghost()
                                        .small()
                                        .label("Remove")
                                        .on_click(move |_, window, cx| {
                                            remove.update(cx, |this, cx| {
                                                this.remove_gateway(name, window, cx)
                                            })
                                        }),
                                )
                            })
                            .child(
                                Button::new("gw-save")
                                    .primary()
                                    .small()
                                    .label("Save")
                                    .on_click(move |_, window, cx| {
                                        save.update(cx, |this, cx| this.save_gateway(window, cx))
                                    }),
                            ),
                    ),
            );
        }
        list = list.child(item);
    }
    v_flex()
        .gap_5()
        .child(heading(
            "Gateways",
            "Every gateway you set up is used at once: its models join the model list, and each message goes to the gateway of the model you pick. Keys stay in keys.json on this machine.",
            cx,
        ))
        .child(list)
}
