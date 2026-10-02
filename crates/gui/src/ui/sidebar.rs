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
                    if page == Page::Jobs {
                        return this.open_jobs(window, cx);
                    }
                    this.page = page;
                    if page == Page::Artifacts {
                        this.read_artifacts(cx);
                    }
                    if page == Page::Plugins {
                        this.reload_plugins();
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

    /// A session row; `picked` rings it as the one the sidebar's keys are on.
    pub(crate) fn session_row(
        &self,
        index: usize,
        now: u64,
        picked: bool,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let c = &self.store.conversations[index];
        let active = index == self.store.active && self.page == Page::Chat;
        let menu = self.session_menu(index, cx);
        let row = h_flex()
            .id(("session", index))
            .relative()
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
            // A run beside the chat holds it.
            .when(self.runs.contains_key(&c.id), |this| {
                this.child(
                    Icon::new(AppIcon::Play)
                        .size_3()
                        .text_color(cx.theme().primary),
                )
            })
            // A notice started a turn there, or a run ended, while you were
            // elsewhere.
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
            .when(picked, |this| {
                this.child(
                    div()
                        .absolute()
                        .inset_0()
                        .rounded(cx.theme().radius)
                        .border_1()
                        .border_color(cx.theme().ring),
                )
            })
            .on_click(cx.listener(move |this, event: &ClickEvent, window, cx| {
                if event.modifiers().shift {
                    this.store.conversations[index].pinned ^= true;
                } else {
                    this.leave_for(index);
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
            .context_menu(menu);
        // Its own id, so each row's menu keeps its own state: sharing one,
        // every row drew the open menu, their shadows stacked into black.
        div().id(("session-menu", index)).child(row)
    }

    /// The sessions the search and the filter let through, by index, the
    /// newest first.
    fn visible_sessions(&self, cx: &App) -> Vec<usize> {
        let query = self.search.read(cx).value().to_lowercase();
        let filter = self.session_filter();
        (0..self.store.conversations.len())
            .rev()
            .filter(|&i| {
                query.is_empty()
                    || self.store.conversations[i]
                        .title
                        .to_lowercase()
                        .contains(&query)
            })
            .filter(|&i| filter.is_none_or(|f| passes(self.session_state(i), f)))
            .collect()
    }

    /// The sessions the sidebar lists, by index, in the order it draws
    /// them: pinned first, then by project or the newest first.
    fn listed_sessions(&self, cx: &App) -> Vec<usize> {
        drawn_order(
            &self.store.conversations,
            self.visible_sessions(cx),
            self.store.settings.group_by_project,
        )
    }

    /// Move the sidebar's keys to the session above, or with `down` below.
    fn pick_session(&mut self, down: bool, cx: &mut Context<Self>) {
        let listed = self.listed_sessions(cx);
        let at = self.picked.and_then(|id| self.store.index_of(id));
        self.picked = move_in(&listed, at, down).map(|index| self.store.conversations[index].id);
        cx.notify();
    }

    /// Open the session the sidebar's keys are on. The keys stay there, as
    /// on a search hit, for the next.
    fn open_picked_session(&mut self, cx: &mut Context<Self>) {
        let Some(index) = self.picked_session(cx) else {
            return;
        };
        self.store.active = index;
        self.error = None;
        self.page = Page::Chat;
        self.follow();
        self.save();
        cx.notify();
    }

    /// The session the sidebar's keys are on, if it is still listed.
    fn picked_session(&self, cx: &App) -> Option<usize> {
        self.picked
            .and_then(|id| self.store.index_of(id))
            .filter(|index| self.listed_sessions(cx).contains(index))
    }

    /// A sidebar key, which acts only while the sidebar itself has the
    /// focus: in its search box, the key is typed instead.
    fn session_key<A: Action>(
        act: fn(&mut Chat, usize, &mut Window, &mut Context<Chat>),
        cx: &Context<Self>,
    ) -> impl Fn(&A, &mut Window, &mut App) + 'static {
        cx.listener(move |this, _: &A, window, cx| {
            if !this.sidebar_focus.is_focused(window) {
                return cx.propagate();
            }
            if let Some(index) = this.picked_session(cx) {
                act(this, index, window, cx);
            }
        })
    }

    /// A sidebar key that moves its keys, which acts only while the sidebar
    /// itself has the focus.
    fn session_move<A: Action>(
        act: fn(&mut Chat, &mut Context<Chat>),
        cx: &Context<Self>,
    ) -> impl Fn(&A, &mut Window, &mut App) + 'static {
        cx.listener(move |this, _: &A, window, cx| {
            if !this.sidebar_focus.is_focused(window) {
                return cx.propagate();
            }
            act(this, cx)
        })
    }

    pub(crate) fn sidebar(&self, window: &Window, cx: &mut Context<Self>) -> impl IntoElement {
        let now = backend::now();
        // Kept while the page is open, so the entry it was opened from stays.
        let session = self.store.conversations[self.store.active].id;
        let workflows =
            self.page == Page::Workflows || !letronna_core::workflow::runs(session).is_empty();
        let visible = self.visible_sessions(cx);
        let keyed = self
            .picked
            .and_then(|id| self.store.index_of(id))
            .filter(|_| self.sidebar_focus.is_focused(window));
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
            sessions = sessions.child(self.session_row(i, now, keyed == Some(i), cx));
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
                    sessions = sessions.child(self.session_row(i, now, keyed == Some(i), cx));
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
                sessions = sessions.child(self.session_row(i, now, keyed == Some(i), cx));
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
            .id("sidebar")
            .track_focus(&self.sidebar_focus)
            .key_context("Sessions")
            // Only F6 gives the sidebar itself the keys; a click leaves
            // them where they were, or with what was clicked.
            .on_any_mouse_down(|_, window, _| window.prevent_default())
            .on_action(Self::session_move::<sessions::PreviousSession>(
                |this, cx| this.pick_session(false, cx),
                cx,
            ))
            .on_action(Self::session_move::<sessions::NextSession>(
                |this, cx| this.pick_session(true, cx),
                cx,
            ))
            .on_action(Self::session_move::<sessions::OpenSession>(
                Chat::open_picked_session,
                cx,
            ))
            .on_action(Self::session_key::<sessions::PinSession>(
                Chat::toggle_pin,
                cx,
            ))
            .on_action(Self::session_key::<sessions::RenameSession>(
                Chat::rename_session,
                cx,
            ))
            .on_action(Self::session_key::<sessions::DeleteSession>(
                Chat::delete_session,
                cx,
            ))
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
                        "plugins",
                        AppIcon::Puzzle,
                        "Plugins",
                        Some(Page::Plugins),
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
                    ))
                    // Only once the session on screen has a run to show.
                    .when(workflows, |this| {
                        this.child(
                            self.nav_item(
                                "workflows",
                                AppIcon::Workflow,
                                "Workflows",
                                Some(Page::Workflows),
                                cx,
                            )
                            .on_click(
                                cx.listener(|this, _, window, cx| this.open_workflows(window, cx)),
                            ),
                        )
                    }),
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
                    .children(self.runs_section(cx))
                    .child(self.section("Sessions", cx))
                    .child(sessions)
                    .children(self.message_hits(cx)),
            )
            .children(self.grain(&self.grains.sidebar))
            .children(self.ring(crate::ui::keys::Region::Sidebar, cx))
    }
}

/// The `visible` sessions in the order the sidebar draws them: pinned
/// first, then by project with `by_project`, else as they come.
fn drawn_order(
    conversations: &[Conversation],
    visible: Vec<usize>,
    by_project: bool,
) -> Vec<usize> {
    let (mut listed, not_pinned): (Vec<usize>, Vec<usize>) =
        visible.into_iter().partition(|&i| conversations[i].pinned);
    if by_project {
        let groups = group_by_project(conversations, &not_pinned);
        listed.extend(groups.into_iter().flat_map(|(_, indices)| indices));
    } else {
        listed.extend(not_pinned);
    }
    listed
}

/// The session after `picked` in `listed`, or with `down` false the one
/// before, no further than the ends; from none, the first.
fn move_in(listed: &[usize], picked: Option<usize>, down: bool) -> Option<usize> {
    let last = listed.len().checked_sub(1)?;
    let at = picked.and_then(|index| listed.iter().position(|&i| i == index));
    let next = match at {
        None => 0,
        Some(at) if down => (at + 1).min(last),
        Some(at) => at.saturating_sub(1),
    };
    Some(listed[next])
}

#[cfg(test)]
mod tests {
    // Not `use super::*`: the prelude re-exports gpui, whose `test` macro
    // would shadow the built-in attribute.
    use super::{drawn_order, move_in};
    use letronna_core::store::Conversation;
    use std::path::PathBuf;

    #[test]
    fn the_sidebar_keys_move_in_its_order_and_stop_at_the_ends() {
        // Listed newest first, so index 7 is at the top.
        let listed = [7, 2, 5];
        assert_eq!(move_in(&listed, Some(7), true), Some(2));
        assert_eq!(move_in(&listed, Some(2), false), Some(7));
        assert_eq!(move_in(&listed, Some(7), false), Some(7));
        assert_eq!(move_in(&listed, Some(5), true), Some(5));
        // From a session the search has hidden, or none, to the top.
        assert_eq!(move_in(&listed, Some(9), true), Some(7));
        assert_eq!(move_in(&listed, None, false), Some(7));
        assert_eq!(move_in(&[], None, true), None);
    }

    #[test]
    fn the_sidebar_keys_go_in_the_order_it_draws_pinned_first() {
        let session = |pinned, workspace: Option<&str>| Conversation {
            pinned,
            workspace: workspace.map(PathBuf::from),
            ..Default::default()
        };
        let conversations = [
            session(false, Some("/code/a")),
            session(false, Some("/code/b")),
            session(true, Some("/code/b")),
            session(false, Some("/code/a")),
        ];
        // As the sidebar has them, newest first.
        let visible = vec![3, 2, 1, 0];
        assert_eq!(
            drawn_order(&conversations, visible.clone(), false),
            [2, 3, 1, 0]
        );
        assert_eq!(drawn_order(&conversations, visible, true), [2, 3, 0, 1]);
    }
}
