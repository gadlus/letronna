use crate::prelude::*;

/// The user guide on the website, opened from the account menu.
const GUIDE: &str = "https://letronna.ai/docs";

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
                    .child(
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
                    )
                    .child(
                        h_flex()
                            .flex_1()
                            .min_w_0()
                            .justify_center()
                            .when(self.page == Page::Chat, |this| {
                                this.child(self.session_title(cx))
                            }),
                    )
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
                            .on_click(
                                cx.listener(|this, _, window, cx| this.toggle_panel(window, cx)),
                            ),
                    )
                    .child(self.account_chip(cx)),
            )
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
            .dropdown_menu_with_anchor(gpui::Corner::TopRight, move |menu, _, _| {
                let (settings, account, auth) = (chat.clone(), chat.clone(), chat.clone());
                menu.min_w(px(180.))
                    .item(
                        PopupMenuItem::new("Account")
                            .icon(Icon::new(IconName::CircleUser))
                            .on_click(move |_, window, cx| {
                                account.update(cx, |this, cx| this.open_account(window, cx))
                            }),
                    )
                    .item(
                        PopupMenuItem::new("Settings")
                            .icon(Icon::new(IconName::Settings))
                            .on_click(move |_, window, cx| {
                                settings.update(cx, |this, cx| this.open_settings(window, cx))
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

    /// The session's project folder. Click to pick or change it.
    fn workspace_chip(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let workspace = &self.store.conversations[self.store.active].workspace;
        let (name, tip) = match workspace {
            Some(p) => (
                p.file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| p.display().to_string()),
                p.display().to_string(),
            ),
            None => ("No project".into(), "Attach a project folder".into()),
        };
        Button::new("workspace")
            .plain(cx)
            .xsmall()
            .text_color(cx.theme().muted_foreground)
            .tooltip(tip)
            .child(
                h_flex()
                    .gap_1()
                    .items_center()
                    .child(Icon::new(AppIcon::Folder).size_3())
                    .child(name),
            )
            .on_click(cx.listener(|this, _, window, cx| this.attach_workspace(window, cx)))
    }

    /// One chip per background job, with a stop button. Only running jobs are
    /// shown: a finished one is history, and the agent can still read its log.
    fn job_chips(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let jobs = letronna_core::jobs::list();
        h_flex().gap_1().children(
            jobs.into_iter()
                .filter(|job| job.running)
                .map(|job| {
                    let label = letronna_core::tools::one_line(&job.command);
                    Button::new(("job", job.id as usize))
                        .plain(cx)
                        .xsmall()
                        .text_color(cx.theme().muted_foreground)
                        .tooltip(format!("Job #{}: {label} — click to stop", job.id))
                        .child(
                            h_flex()
                                .gap_1()
                                .items_center()
                                .max_w(px(180.))
                                .child(div().size(px(6.)).rounded_full().bg(cx.theme().green))
                                .child(div().truncate().child(label))
                                .child(Icon::new(IconName::Close).size_3()),
                        )
                        .on_click(cx.listener(move |_, _, _, cx| {
                            let _ = letronna_core::jobs::stop(job.id);
                            cx.notify();
                        }))
                })
                .collect::<Vec<_>>(),
        )
    }

    pub(crate) fn status_bar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        h_flex()
            .h(px(20.))
            .px_3()
            .gap_2()
            .items_center()
            .text_xs()
            .text_color(cx.theme().muted_foreground)
            .child(self.gateway_chip(cx))
            .child(self.workspace_chip(cx))
            .child(self.job_chips(cx))
            .child(div().flex_1())
            .children(self.update_chip(cx))
            .child(concat!("v", env!("CARGO_PKG_VERSION")))
            .child(
                div()
                    .text_color(cx.theme().muted_foreground.opacity(0.6))
                    .child(env!("GIT_HASH")),
            )
    }
}
