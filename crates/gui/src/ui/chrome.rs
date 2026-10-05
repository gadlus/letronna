use crate::prelude::*;
use gpui_component::popover::Popover;

/// The user guide on the website, opened from the account menu.
const GUIDE: &str = "https://letronna.ai/docs";
/// The window width from which the palette's button says what it does:
/// below it, its words would take the session name's room.
const COMMAND_WORDS: Pixels = px(960.);
/// The chat pane's width from which the memory chip says what it is set
/// to: below it, the words would push the status bar past its edge.
const MEMORY_WORDS: Pixels = px(760.);

impl Chat {
    pub(crate) fn title_bar(&self, window: &Window, cx: &mut Context<Self>) -> impl IntoElement {
        // On macOS the bar starts after the traffic lights: three 12px buttons
        // 20px apart from x=12 end at 64, so leave the same 12px gap after them
        // that the other platforms have at the edge. Fullscreen hides the lights
        // and the bar adds that 12px by itself.
        let inset = if window.is_fullscreen() {
            px(0.)
        } else {
            px(76.)
        };
        TitleBar::new()
            .when(cfg!(target_os = "macos"), |bar| bar.pl(inset))
            .child(
                h_flex()
                    .flex_1()
                    .items_center()
                    .gap_2()
                    // Nothing follows the chip on macOS, so keep it off the edge.
                    .when(cfg!(target_os = "macos"), |bar| bar.pr_3())
                    // Both sides grow alike from nothing, so the name sits
                    // in the middle of the bar, not of what the buttons
                    // leave; a side wider than its half only pushes it over.
                    .child(
                        h_flex().flex_1().items_center().child(
                            Button::new("toggle-sidebar")
                                .plain(cx)
                                .small()
                                .icon(
                                    Icon::new(if self.sidebar_open {
                                        IconName::PanelLeftClose
                                    } else {
                                        IconName::PanelLeftOpen
                                    })
                                    .size_4(),
                                )
                                .tooltip_with_action("Toggle sidebar", &ToggleSidebar, None)
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.sidebar_open = !this.sidebar_open;
                                    cx.notify();
                                })),
                        ),
                    )
                    .child(
                        h_flex()
                            .min_w_0()
                            .justify_center()
                            .when(self.page == Page::Chat, |this| {
                                this.child(self.session_title(cx))
                            }),
                    )
                    .child(
                        h_flex()
                            .flex_1()
                            .items_center()
                            .justify_end()
                            .gap_2()
                            .child(self.command_button(window, cx))
                            .child(
                                Button::new("toggle-panel")
                                    .plain(cx)
                                    .small()
                                    .icon(Icon::new(IconName::PanelRight).size_4())
                                    .tooltip_with_action(
                                        "Toggle right panel",
                                        &crate::ui::panel::TogglePanel,
                                        Some("Chat"),
                                    )
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.toggle_panel(window, cx)
                                    })),
                            )
                            .children(self.profile_label.clone().map(|name| {
                                div()
                                    .text_xs()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(name)
                            }))
                            .child(self.account_chip(cx)),
                    ),
            )
    }

    /// A quiet field that opens the palette, so the palette, and the key
    /// for it, can be found without knowing it is there.
    fn command_button(&self, window: &Window, cx: &mut Context<Self>) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
        let key = crate::ui::keys::bound_key(&crate::ui::palette::OpenPalette, cx);
        // In a narrow window the words go first, so the session's name
        // keeps its room; the glass and the key say enough.
        let words = window.viewport_size().width >= COMMAND_WORDS;
        Button::new("command")
            .framed(cx)
            .xsmall()
            .px_2()
            .child(
                h_flex()
                    .gap_2()
                    .items_center()
                    .text_xs()
                    .text_color(muted)
                    .child(Icon::new(AppIcon::Search).size_3())
                    .when(words, |this| this.child("Search or run a command"))
                    .children(key.map(|key| Kbd::format(&key))),
            )
            .when(!words, |this| this.tooltip("Search or run a command"))
            .on_click(cx.listener(|this, _, window, cx| this.open_palette(window, cx)))
    }

    /// The open session's name, so it is clear which one this is; click
    /// to rename it.
    fn session_title(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let index = self.store.active;
        let title = self.store.conversations[index].title.clone();
        Button::new("session-title")
            .plain(cx)
            .xsmall()
            .tooltip("Rename")
            // Set as the window's title, not as a hint beside it.
            .child(
                div()
                    .max_w(px(420.))
                    .truncate()
                    .text_sm()
                    .font_weight(FontWeight::MEDIUM)
                    .child(title),
            )
            .on_click(
                cx.listener(move |this, _, window, cx| this.rename_session(index, window, cx)),
            )
    }

    fn account_chip(&self, cx: &mut Context<Self>) -> impl IntoElement {
        // The same account the terminal signs in to; nobody signed in is a guest.
        let name: SharedString = self
            .account
            .as_ref()
            .map(|a| a.name.split(' ').next().unwrap_or("").to_owned())
            .filter(|n| !n.is_empty())
            .unwrap_or_else(|| "Guest".into())
            .into();
        let signed_in = self.account.is_some();
        // While the browser has the sign-in, the chip says so.
        let waiting = self.sign_in_url.is_some();
        let name: SharedString = if waiting && !signed_in {
            "Signing in…".into()
        } else {
            name
        };
        let chat = cx.entity();
        Button::new("account")
            .plain(cx)
            .small()
            .compact()
            .child(
                h_flex()
                    .gap_1()
                    .items_center()
                    .child(
                        Icon::new(IconName::CircleUser)
                            .size_4()
                            .text_color(cx.theme().muted_foreground),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(name),
                    )
                    .child(
                        Icon::new(IconName::ChevronDown)
                            .size_3()
                            .text_color(cx.theme().muted_foreground),
                    ),
            )
            .dropdown_menu_with_anchor(gpui::Anchor::TopRight, move |menu, window, cx| {
                let (settings, account, auth) = (chat.clone(), chat.clone(), chat.clone());
                let (usage, setup) = (chat.clone(), chat.clone());
                let menu = menu
                    .min_w(px(180.))
                    .item(
                        PopupMenuItem::new("Account")
                            .icon(Icon::new(IconName::CircleUser))
                            .on_click(move |_, window, cx| {
                                account.update(cx, |this, cx| this.open_account(window, cx))
                            }),
                    )
                    .item(
                        PopupMenuItem::new("Usage")
                            .icon(Icon::new(AppIcon::Clock))
                            .on_click(move |_, window, cx| {
                                usage.update(cx, |this, cx| this.open_usage(window, cx))
                            }),
                    )
                    .item(
                        PopupMenuItem::new("Settings")
                            .icon(Icon::new(IconName::Settings))
                            .on_click(move |_, window, cx| {
                                settings.update(cx, |this, cx| this.open_settings(window, cx))
                            }),
                    );
                crate::ui::profiles::menu(menu, window, cx, chat.clone())
                    .item(
                        PopupMenuItem::new("Set up Letronna")
                            .icon(Icon::new(AppIcon::Sparkles))
                            .on_click(move |_, _, cx| {
                                setup.update(cx, |this, cx| this.open_setup(cx))
                            }),
                    )
                    .item(
                        PopupMenuItem::new("Guide")
                            .icon(Icon::new(IconName::Info))
                            .on_click(|_, _, cx| cx.open_url(GUIDE)),
                    )
                    .separator()
                    .item(
                        PopupMenuItem::new(if signed_in {
                            "Sign out"
                        } else if waiting {
                            "Open the sign-in page again"
                        } else {
                            "Sign in"
                        })
                        .icon(Icon::new(IconName::CircleUser))
                        .on_click(move |_, window, cx| {
                            auth.update(cx, |this, cx| {
                                if signed_in {
                                    this.sign_out(cx)
                                } else {
                                    // On the Account page, where the wait
                                    // for the browser shows and can be
                                    // cancelled or copied elsewhere.
                                    this.open_account(window, cx);
                                    this.sign_in(cx)
                                }
                            })
                        }),
                    )
            })
    }

    fn gateway_chip(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let (dot, state, tip) = if self.loading_models {
            (
                cx.theme().muted_foreground,
                "checking",
                "Loading every gateway's models".to_string(),
            )
        } else if let Some(e) = &self.gateway_error {
            (cx.theme().danger, "error", e.clone())
        } else {
            (cx.theme().green, "ready", {
                let mut gateways: Vec<&str> = self.models.iter().map(|(g, _)| g.as_str()).collect();
                gateways.dedup();
                format!("{} models from {}", self.models.len(), gateways.join(", "))
            })
        };
        Button::new("gateway")
            .plain(cx)
            .xsmall()
            .text_color(cx.theme().muted_foreground)
            .tooltip(tip)
            .child(
                h_flex()
                    .gap_1p5()
                    .items_center()
                    .child(div().size(px(6.)).rounded_full().bg(dot))
                    .child(div().text_color(cx.theme().foreground).child("Gateway"))
                    .child(state),
            )
            .on_click(cx.listener(|this, _, window, cx| this.open_settings(window, cx)))
    }

    /// The session's project folder, with its branch and how many files
    /// changed when it is in git. Click to see the changes, or to pick a
    /// folder when there is none.
    fn workspace_chip(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let workspace = &self.store.conversations[self.store.active].workspace;
        let Some(folder) = workspace else {
            return Button::new("workspace")
                .plain(cx)
                .xsmall()
                .text_color(cx.theme().muted_foreground)
                .tooltip("Attach a project folder")
                .child(
                    h_flex()
                        .gap_1()
                        .items_center()
                        .child(Icon::new(AppIcon::Folder).size_3())
                        .child("No project"),
                )
                .on_click(cx.listener(|this, _, window, cx| this.attach_workspace(window, cx)));
        };
        let name = folder
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| folder.display().to_string());
        let status = self.git_status();
        let (icon, label) = match status {
            Some(status) => (AppIcon::Branch, format!("{name} · {}", status.describe())),
            None => (AppIcon::Folder, name),
        };
        Button::new("workspace")
            .plain(cx)
            .xsmall()
            .text_color(cx.theme().muted_foreground)
            .tooltip_with_action(
                format!("{}: see the changes", folder.display()),
                &crate::ui::changes::ChangesTab,
                Some("Chat"),
            )
            .child(
                h_flex()
                    .gap_1()
                    .items_center()
                    .child(Icon::new(icon).size_3())
                    .child(label),
            )
            .on_click(cx.listener(|this, _, window, cx| this.show_changes(window, cx)))
    }

    /// How many things are working, away from the chat, whose strip over the
    /// composer lists them; click to see them there.
    fn running_pill(&self, cx: &mut Context<Self>) -> Option<impl IntoElement> {
        if self.page == Page::Chat {
            return None;
        }
        let running = self.running_count();
        if running == 0 {
            return None;
        }
        Some(
            Button::new("running")
                .plain(cx)
                .xsmall()
                .text_color(cx.theme().muted_foreground)
                .tooltip_with_action(
                    "See what is running",
                    &crate::ui::activity::ShowRunning,
                    Some("Chat"),
                )
                .child(
                    h_flex()
                        .gap_1()
                        .items_center()
                        .child(div().size(px(6.)).rounded_full().bg(cx.theme().green))
                        .child(format!("{running} running")),
                )
                .on_click(cx.listener(|this, _, window, cx| this.show_running(window, cx))),
        )
    }

    /// The loop in the session on screen, if it has one, with a stop button.
    fn loop_chip(&self, cx: &mut Context<Self>) -> Option<impl IntoElement> {
        let session = self.store.conversations[self.store.active].id;
        let about = letronna_core::wake::looping(session)?;
        Some(
            Button::new("loop")
                .plain(cx)
                .xsmall()
                .text_color(cx.theme().muted_foreground)
                .tooltip(format!("{about} — click to stop"))
                .child(
                    h_flex()
                        .gap_1()
                        .items_center()
                        .child(Icon::new(AppIcon::Reload).size_3())
                        .child("Loop")
                        .child(Icon::new(IconName::Close).size_3()),
                )
                .on_click(cx.listener(move |_, _, _, cx| {
                    letronna_core::wake::stop(session);
                    cx.notify();
                })),
        )
    }

    /// The bar under the chat, as wide as `pane`.
    pub(crate) fn status_bar(&self, pane: Pixels, cx: &mut Context<Self>) -> impl IntoElement {
        h_flex()
            .h(px(20.))
            .px_3()
            .gap_2()
            .items_center()
            .text_xs()
            .text_color(cx.theme().muted_foreground)
            .child(self.gateway_chip(cx))
            .child(self.workspace_chip(cx))
            .child(self.memory_chip(pane >= MEMORY_WORDS, cx))
            .children(self.running_pill(cx))
            .children(self.loop_chip(cx))
            .children(
                self.heard_elsewhere
                    .as_ref()
                    .map(|holder| div().min_w_0().truncate().child(holder.line())),
            )
            .children(self.context_chip(cx))
            .child(div().flex_1())
            .children(self.update_chip(cx))
            .child(concat!("v", env!("CARGO_PKG_VERSION")))
            .child(
                div()
                    .text_color(cx.theme().muted_foreground.opacity(0.6))
                    .child(env!("GIT_HASH")),
            )
    }

    /// How full the active session's context is, from its latest reply,
    /// which is what a request actually carried rather than an estimate of
    /// it. Click, or open with the keyboard, for the breakdown. `None`
    /// before there is a reply to read it from.
    fn context_chip(&self, cx: &mut Context<Self>) -> Option<impl IntoElement> {
        let reply = self.store.conversations[self.store.active]
            .messages
            .iter()
            .rev()
            .find(|m| m.role == "assistant" && m.window > 0 && m.input_tokens() > 0)?;
        let share = reply.input_tokens().saturating_mul(100) / reply.window;
        let chip = Button::new("context")
            .plain(cx)
            .xsmall()
            .text_color(cx.theme().muted_foreground)
            .tooltip("What fills the context")
            .child(format!("{share}%"))
            // The popover opens on a mouse press only, so a key opens it here.
            .on_click(cx.listener(|this, event: &ClickEvent, _, cx| {
                if event.is_keyboard() {
                    this.open_context_popover(cx);
                }
            }));
        let (chat, report) = (cx.entity(), self.context_report);
        Some(
            Popover::new("context-popover")
                .anchor(gpui::Anchor::BottomRight)
                // The built-in appearance shadows the panel; a hairline
                // border says the same thing without one.
                .appearance(false)
                .open(self.context_report.is_some())
                .on_open_change(move |open, _, cx| {
                    chat.update(cx, |this, cx| {
                        if *open {
                            this.open_context_popover(cx);
                        } else {
                            this.context_report = None;
                            cx.notify();
                        }
                    })
                })
                .trigger(chip)
                .content(move |_, _, cx| context_popover(report, cx)),
        )
    }

    /// Add up what fills the active session's context afresh, for the
    /// popover to show. Reading memory and skills off disk and listing the
    /// tools is done off the window's thread, so a slow disk or a big
    /// project cannot stall a frame; the popover opens when it is done.
    pub(crate) fn open_context_popover(&mut self, cx: &mut Context<Self>) {
        if self.context_report.is_some() {
            return;
        }
        let ix = self.store.active;
        let mut connection = match self.connection_for(ix, None) {
            Ok(connection) => connection,
            Err(error) => {
                self.error = Some(error);
                cx.notify();
                return;
            }
        };
        connection.workspace = self.store.conversations[ix].workspace.clone();
        let session = self.store.conversations[ix].id;
        let messages = self.store.conversations[ix].messages.clone();
        let floor = self.store.conversations[ix].floor();
        let added = cx
            .background_executor()
            .spawn(async move { backend::context_report(&connection, &messages, floor) });
        cx.spawn(async move |this, cx| {
            let report = added.await;
            let _ = this.update(cx, |this, cx| {
                // The user may have moved to another session meanwhile.
                if this.store.conversations[this.store.active].id == session {
                    this.context_report = Some(report);
                    cx.notify();
                }
            });
        })
        .detach();
    }
}

/// The popover's content: the total against the window, a bar split by what
/// filled it, and a line for each part. `report` is read once, when the
/// popover is opened; see `open_context_popover`.
fn context_popover(report: Option<backend::ContextReport>, cx: &App) -> Div {
    let muted = cx.theme().muted_foreground;
    // Like `popover_style`, but a hairline border instead of its shadow.
    let panel = |content: Div| {
        content
            .w(px(260.))
            .bg(cx.theme().popover)
            .text_color(cx.theme().popover_foreground)
            .border_1()
            .border_color(cx.theme().border)
            .rounded(cx.theme().radius)
            .p_3()
    };
    let Some(report) = report else {
        return panel(div().child(div().text_sm().text_color(muted).child("Adding up…")));
    };
    let parts = report.parts;
    let rows = [
        ("System", parts.system, cx.theme().chart_1),
        ("Tools", parts.tools, cx.theme().chart_2),
        ("MCP", parts.mcp, cx.theme().chart_2.opacity(0.5)),
        ("Memory", parts.memory, cx.theme().chart_3),
        ("Skills", parts.skills, cx.theme().chart_4),
        ("Conversation", parts.conversation, cx.theme().chart_5),
    ];
    let total = parts.total().max(1);
    let bar = rows.iter().fold(
        h_flex()
            .w_full()
            .h(px(6.))
            .rounded_full()
            .overflow_hidden()
            .bg(cx.theme().border),
        |bar, (_, value, color)| {
            bar.child(
                div()
                    .h_full()
                    .w(relative(*value as f32 / total as f32))
                    .bg(*color),
            )
        },
    );
    panel(
        v_flex()
            .gap_3()
            .child(
                div()
                    .text_sm()
                    .font_weight(FontWeight::MEDIUM)
                    .child(if report.window == 0 {
                        // A model whose window is not known has no "of".
                        letronna_core::usage::count(report.used)
                    } else {
                        format!(
                            "{} of {}",
                            letronna_core::usage::count(report.used),
                            letronna_core::usage::count(report.window)
                        )
                    }),
            )
            .child(bar)
            .children(rows.into_iter().filter(|(_, value, _)| *value > 0).map(
                |(name, value, color)| {
                    h_flex()
                        .gap_2()
                        .items_center()
                        .text_xs()
                        .child(div().size(px(6.)).rounded_full().flex_shrink_0().bg(color))
                        .child(div().flex_1().text_color(muted).child(name))
                        .child(letronna_core::usage::count(value))
                },
            )),
    )
}
