use super::sessions::{self, SessionFilter, group_by_project, passes, project_label};
use crate::prelude::*;

/// The sidebar's width, its border included, which the chat column is
/// sized from.
pub(crate) const SIDEBAR_WIDTH: Pixels = px(250.);

impl Chat {
    pub(crate) fn nav_item(
        &self,
        id: &'static str,
        icon: AppIcon,
        label: &'static str,
        page: Option<Page>,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let active = page.is_some_and(|p| p == self.page);
        h_flex()
            .id(id)
            .h(px(28.))
            .px_2()
            .gap_2()
            .items_center()
            .rounded(cx.theme().radius)
            .cursor_pointer()
            .font_family(HEADING_FONT)
            .font_weight(FontWeight::MEDIUM)
            .text_color(cx.theme().sidebar_foreground)
            .when(active, |this| this.bg(cx.theme().sidebar_accent))
            .active(|s| s.bg(cx.theme().sidebar_accent.opacity(0.6)))
            .child(
                Icon::new(icon)
                    .size_4()
                    .text_color(cx.theme().muted_foreground),
            )
            .child(div().flex_1().child(label))
            .when_some(page, |this, page| {
                this.on_click(cx.listener(move |this, _, window, cx| {
                    if page == Page::Capabilities {
                        return this.open_capabilities(window, cx);
                    }
                    this.page = page;
                    if page == Page::Artifacts {
                        this.read_artifacts(cx);
                    }
                    this.focus_composer(window, cx);
                    cx.notify();
                }))
            })
    }

    pub(crate) fn section(&self, label: &'static str, cx: &Context<Self>) -> Div {
        div()
            .mt_3()
            .mb_1()
            .px_2()
            .text_xs()
            .font_family(HEADING_FONT)
            .font_weight(FontWeight::MEDIUM)
            .text_color(cx.theme().muted_foreground)
            .child(label)
    }

    pub(crate) fn session_row(
        &self,
        index: usize,
        now: u64,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let c = &self.store.conversations[index];
        let active = index == self.store.active && self.page == Page::Chat;
        let menu = self.session_menu(index, cx);
        h_flex()
            .id(("session", index))
            .h(px(26.))
            .px_2()
            .gap_2()
            .items_center()
            .rounded(cx.theme().radius)
            .cursor_pointer()
            .text_sm()
            .when(active, |this| this.bg(cx.theme().sidebar_accent))
            .active(|s| s.bg(cx.theme().sidebar_accent.opacity(0.6)))
            .child(if c.pinned {
                Icon::new(AppIcon::Pin)
                    .size_3()
                    .text_color(if active {
                        cx.theme().primary
                    } else {
                        cx.theme().muted_foreground
                    })
                    .into_any_element()
            } else {
                div()
                    .size(px(4.))
                    .rounded_full()
                    .flex_shrink_0()
                    .bg(if active {
                        cx.theme().primary
                    } else {
                        cx.theme().muted_foreground.opacity(0.5)
                    })
                    .into_any_element()
            })
            .child(div().flex_1().min_w_0().truncate().child(c.title.clone()))
            // A notice started a turn there while you were elsewhere.
            .when(self.unseen.contains(&c.id), |this| {
                this.child(
                    div()
                        .size(px(6.))
                        .rounded_full()
                        .flex_shrink_0()
                        .bg(cx.theme().primary),
                )
            })
            // Green, as a running job's chip is: a helper works for it.
            .when(self.helpers_working_in(c.id), |this| {
                this.child(
                    div()
                        .size(px(6.))
                        .rounded_full()
                        .flex_shrink_0()
                        .bg(cx.theme().green),
                )
            })
            // The session a reply is running in says so, on screen or not.
            .child(if self.busy && self.streaming_id == Some(c.id) {
                // In the age's colour: the primary is lost on the sidebar.
                gpui_component::spinner::Spinner::new()
                    .xsmall()
                    .color(cx.theme().muted_foreground)
                    .into_any_element()
            } else {
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(age_label(c.updated, now))
                    .into_any_element()
            })
            .on_click(cx.listener(move |this, event: &ClickEvent, window, cx| {
                if event.modifiers().shift {
                    this.store.conversations[index].pinned ^= true;
                } else {
                    this.store.active = index;
                    this.error = None;
                    this.page = Page::Chat;
                    // The handle is shared across sessions; start at the latest message.
                    this.follow();
                    this.focus_composer(window, cx);
                }
                this.save();
                cx.notify();
            }))
            .context_menu(menu)
    }

    pub(crate) fn sidebar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let now = backend::now();
        let query = self.search.read(cx).value().to_lowercase();
        let filter = self.session_filter();
        let visible: Vec<usize> = (0..self.store.conversations.len())
            .rev()
            .filter(|&i| {
                query.is_empty()
                    || self.store.conversations[i]
                        .title
                        .to_lowercase()
                        .contains(&query)
            })
            .filter(|&i| filter.is_none_or(|f| passes(self.session_state(i), f)))
            .collect();
        let pinned: Vec<usize> = visible
            .iter()
            .copied()
            .filter(|&i| self.store.conversations[i].pinned)
            .collect();
        let not_pinned: Vec<usize> = visible
            .iter()
            .copied()
            .filter(|&i| !self.store.conversations[i].pinned)
            .collect();
        let mut sessions = v_flex().gap_0p5();
        for &i in &pinned {
            sessions = sessions.child(self.session_row(i, now, cx));
        }
        if self.store.settings.group_by_project {
            for (workspace, indices) in group_by_project(&self.store.conversations, &not_pinned) {
                sessions = sessions.child(
                    div()
                        .mt_2()
                        .mb_0p5()
                        .px_2()
                        .text_xs()
                        .font_family(HEADING_FONT)
                        .text_color(cx.theme().muted_foreground)
                        .child(project_label(workspace.as_deref())),
                );
                for i in indices {
                    sessions = sessions.child(self.session_row(i, now, cx));
                }
            }
        } else {
            let mut last_bucket = "";
            for &i in &not_pinned {
                let b = bucket(self.store.conversations[i].updated, now);
                if b != last_bucket {
                    last_bucket = b;
                    sessions = sessions.child(
                        div()
                            .mt_2()
                            .mb_0p5()
                            .px_2()
                            .text_xs()
                            .font_family(HEADING_FONT)
                            .text_color(cx.theme().muted_foreground)
                            .child(b),
                    );
                }
                sessions = sessions.child(self.session_row(i, now, cx));
            }
        }
        if visible.is_empty() {
            sessions = sessions.child(
                div()
                    .px_2()
                    .py_1()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child("No sessions match"),
            );
        }

        v_flex()
            .key_context("Sessions")
            .on_action(cx.listener(|this, _: &sessions::FilterNeedsYou, _, cx| {
                this.toggle_session_filter(SessionFilter::NeedsYou, cx);
            }))
            .on_action(cx.listener(|this, _: &sessions::FilterWorking, _, cx| {
                this.toggle_session_filter(SessionFilter::Working, cx);
            }))
            .on_action(cx.listener(|this, _: &sessions::FilterUnseen, _, cx| {
                this.toggle_session_filter(SessionFilter::Unseen, cx);
            }))
            .on_action(cx.listener(|this, _: &sessions::FilterPinned, _, cx| {
                this.toggle_session_filter(SessionFilter::Pinned, cx);
            }))
            .on_action(cx.listener(|this, _: &sessions::ToggleGrouping, _, cx| {
                this.toggle_grouping(cx);
            }))
            .w(SIDEBAR_WIDTH)
            .h_full()
            .flex_shrink_0()
            .relative()
            .bg(cx.theme().sidebar)
            .text_color(cx.theme().sidebar_foreground)
            .border_r(px(3.))
            .border_color(cx.theme().sidebar_border)
            .child(
                v_flex()
                    .px_2()
                    .pt_2()
                    .gap_0p5()
                    .child(
                        self.nav_item("new", AppIcon::SquarePen, "New session", None, cx)
                            .child(Kbd::new(Keystroke::parse("secondary-n").unwrap()).text_xs())
                            .on_click(cx.listener(|this, _, window, cx| this.new_chat(window, cx))),
                    )
                    .child(self.nav_item(
                        "capabilities",
                        AppIcon::Sparkles,
                        "Capabilities",
                        Some(Page::Capabilities),
                        cx,
                    ))
                    .child(self.nav_item(
                        "messaging",
                        AppIcon::MessageSquare,
                        "Messaging",
                        Some(Page::Messaging),
                        cx,
                    ))
                    .child(self.nav_item(
                        "artifacts",
                        AppIcon::File,
                        "Artifacts",
                        Some(Page::Artifacts),
                        cx,
                    ))
                    .child(
                        self.nav_item(
                            "activity",
                            AppIcon::Robot,
                            "Activity",
                            Some(Page::Activity),
                            cx,
                        )
                        // To the list, as the pill goes, even from a transcript.
                        .on_click(
                            cx.listener(|this, _, window, cx| this.open_activity(window, cx)),
                        ),
                    )
                    .child(self.nav_item(
                        "jobs",
                        AppIcon::Clock,
                        "Scheduled jobs",
                        Some(Page::Jobs),
                        cx,
                    )),
            )
            .child(
                div().px_2().pt_3().child(
                    Input::new(&self.search)
                        .xsmall()
                        .h(px(28.))
                        .px_2()
                        .appearance(false)
                        .prefix(
                            Icon::new(AppIcon::Search)
                                .size_3()
                                .text_color(cx.theme().muted_foreground),
                        ),
                ),
            )
            .child(div().pt_1().child(self.session_filter_row(cx)))
            .child(
                v_flex()
                    .id("session-list")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .px_2()
                    .child(self.section("Sessions", cx))
                    .child(sessions)
                    .children(self.message_hits(cx)),
            )
            .children(self.grain(&self.grains.sidebar))
    }
}
