use crate::app::card_is_up;
use crate::prelude::*;
use gpui_component::{Selectable, menu::PopupMenu};

actions!(
    sessions,
    [
        FilterNeedsYou,
        FilterWorking,
        FilterUnseen,
        FilterPinned,
        ToggleGrouping,
        PreviousSession,
        NextSession,
        OpenSession,
        PinSession,
        RenameSession,
        DeleteSession
    ]
);

/// One of the sidebar's filters, narrowing the list to sessions in that
/// state; only one applies at a time.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum SessionFilter {
    /// A permission prompt or an `ask_user` question is waiting on you.
    NeedsYou,
    /// A turn is running, here or in a helper.
    Working,
    /// A notice woke it while it was not on screen.
    Unseen,
    Pinned,
}

impl SessionFilter {
    pub(crate) const ALL: [SessionFilter; 4] = [
        SessionFilter::NeedsYou,
        SessionFilter::Working,
        SessionFilter::Unseen,
        SessionFilter::Pinned,
    ];

    fn as_str(self) -> &'static str {
        match self {
            SessionFilter::NeedsYou => "needs_you",
            SessionFilter::Working => "working",
            SessionFilter::Unseen => "unseen",
            SessionFilter::Pinned => "pinned",
        }
    }

    fn parse(saved: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|f| f.as_str() == saved)
    }

    fn label(self) -> &'static str {
        match self {
            SessionFilter::NeedsYou => "Needs you",
            SessionFilter::Working => "Working",
            SessionFilter::Unseen => "Unseen",
            SessionFilter::Pinned => "Pinned",
        }
    }

    fn icon(self) -> AppIcon {
        match self {
            SessionFilter::NeedsYou => AppIcon::Alert,
            SessionFilter::Working => AppIcon::Loader,
            SessionFilter::Unseen => AppIcon::Circle,
            SessionFilter::Pinned => AppIcon::Pin,
        }
    }
}

/// A session's state as the sidebar's filters see it.
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub(crate) struct SessionState {
    pub(crate) needs_you: bool,
    pub(crate) working: bool,
    pub(crate) unseen: bool,
    pub(crate) pinned: bool,
}

/// Whether a session in `state` passes `filter`.
pub(crate) fn passes(state: SessionState, filter: SessionFilter) -> bool {
    match filter {
        SessionFilter::NeedsYou => state.needs_you,
        SessionFilter::Working => state.working,
        SessionFilter::Unseen => state.unseen,
        SessionFilter::Pinned => state.pinned,
    }
}

/// The filter after `picked` is chosen while `current` applies: picking the
/// one already on clears it, so pressing a filter's key twice turns it off.
fn next_filter(current: Option<SessionFilter>, picked: SessionFilter) -> Option<SessionFilter> {
    if current == Some(picked) {
        None
    } else {
        Some(picked)
    }
}

/// `indices` into `conversations`, grouped by project folder: each group
/// keeps the order its sessions already had, and groups are ordered by
/// which project's session came first. Sessions with no folder share one
/// group, keyed by `None`.
pub(crate) fn group_by_project(
    conversations: &[Conversation],
    indices: &[usize],
) -> Vec<(Option<PathBuf>, Vec<usize>)> {
    let mut groups: Vec<(Option<PathBuf>, Vec<usize>)> = Vec::new();
    for &i in indices {
        let key = conversations[i].workspace.clone();
        match groups.iter_mut().find(|(k, _)| *k == key) {
            Some((_, group)) => group.push(i),
            None => groups.push((key, vec![i])),
        }
    }
    groups
}

/// The heading a project's group shows: its folder's name, its full path
/// when it has none, or "No project" for sessions with no folder.
pub(crate) fn project_label(workspace: Option<&std::path::Path>) -> String {
    match workspace {
        Some(path) => path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.display().to_string()),
        None => "No project".to_owned(),
    }
}

impl Chat {
    pub(crate) fn session_menu(
        &self,
        index: usize,
        cx: &mut Context<Self>,
    ) -> impl Fn(PopupMenu, &mut Window, &mut Context<PopupMenu>) -> PopupMenu + 'static {
        let chat = cx.entity();
        let pinned = self.store.conversations[index].pinned;
        move |menu, _, _| {
            let item =
                |label: &'static str,
                 icon: AppIcon,
                 f: fn(&mut Chat, usize, &mut Window, &mut Context<Chat>)| {
                    let chat = chat.clone();
                    PopupMenuItem::new(label).icon(Icon::new(icon)).on_click(
                        move |_, window, cx| chat.update(cx, |this, cx| f(this, index, window, cx)),
                    )
                };
            menu.min_w(px(200.))
                .item(item("Rename", AppIcon::Pencil, Chat::rename_session))
                .item(item(
                    if pinned { "Unpin" } else { "Pin" },
                    AppIcon::Pin,
                    Chat::toggle_pin,
                ))
                .item(item("Decisions", AppIcon::Shield, Chat::show_decisions))
                .separator()
                .item(item(
                    "Export as Markdown",
                    AppIcon::Export,
                    Chat::export_session,
                ))
                .item(item("Import a session", AppIcon::PopIn, Chat::open_import))
                .separator()
                .item(item("Delete", AppIcon::Trash, Chat::delete_session))
        }
    }

    pub(crate) fn toggle_pin(&mut self, index: usize, _: &mut Window, cx: &mut Context<Self>) {
        self.store.conversations[index].pinned ^= true;
        self.save();
        cx.notify();
    }

    pub(crate) fn rename_session(
        &mut self,
        index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let title = self.store.conversations[index].title.clone();
        self.rename
            .update(cx, |input, cx| input.set_value(title, window, cx));
        let input = self.rename.clone();
        let chat = cx.entity();
        window.open_dialog(cx, move |dialog, _, _| {
            let input = input.clone();
            let chat = chat.clone();
            dialog
                .title(div().font_family(HEADING_FONT).child("Rename session"))
                .w(px(420.))
                .confirm()
                .child(Input::new(&input))
                .on_ok(move |_, _, cx| {
                    let title = input.read(cx).value().trim().to_string();
                    chat.update(cx, |this, cx| {
                        if !title.is_empty() {
                            this.store.conversations[index].title = title;
                            this.save();
                            cx.notify();
                        }
                    });
                    true
                })
        });
        // After opening, which takes focus for the dialog itself; the name
        // is what is being edited, so the cursor belongs there.
        self.rename.update(cx, |input, cx| input.focus(window, cx));
    }

    pub(crate) fn delete_session(
        &mut self,
        index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let title = self.store.conversations[index].title.clone();
        let chat = cx.entity();
        super::confirm_delete(
            "Delete session",
            format!("\"{title}\" and its messages will be removed from this device."),
            window,
            cx,
            move |window, cx| {
                chat.update(cx, |this, cx| {
                    if this.store.conversations.len() <= index {
                        return;
                    }
                    // Said out loud: the dialog closes either way, and closing
                    // it on nothing looked like the delete had been lost.
                    if this.busy {
                        this.error = Some("Stop the reply before deleting a session.".into());
                        cx.notify();
                        return;
                    }
                    // Its id can be handed out again before the summary lands.
                    if this
                        .compacting
                        .contains(&this.store.conversations[index].id)
                    {
                        this.error =
                            Some("Wait for the summary before deleting the session.".into());
                        cx.notify();
                        return;
                    }
                    // Its end would come for a session no longer there.
                    if this.runs.contains_key(&this.store.conversations[index].id) {
                        this.error = Some("Stop its run before deleting this session.".into());
                        cx.notify();
                        return;
                    }
                    // Through the store, so the session's file goes too,
                    // and its private browser and draft with it.
                    let id = this.store.conversations[index].id;
                    this.stop_review(id);
                    letronna_core::browser::forget(id);
                    this.drafts.remove(&id);
                    this.open.retain(|&open| open != id);
                    this.recent.retain(|&recent| recent != id);
                    this.panel.forget(id);
                    // The next new session can be given this id again, and
                    // must not start out with what was allowed here, nor in
                    // this one's mode.
                    this.session_allow
                        .remove(&this.store.conversations[index].id);
                    letronna_core::approvals::forget_session(this.store.conversations[index].id);
                    // Nor with this one's loop, whose ticks would go there.
                    letronna_core::wake::stop(this.store.conversations[index].id);
                    this.modes.remove(&this.store.conversations[index].id);
                    // Nor show its helpers' cards; dropped, they are declined.
                    let id = this.store.conversations[index].id;
                    this.agent_asks.retain(|request| request.root != id);
                    this.store.remove(index);
                    if this.store.conversations.is_empty() {
                        this.new_chat(window, cx);
                        return;
                    }
                    if this.store.active >= index && this.store.active > 0 {
                        this.store.active -= 1;
                    }
                    // Its files leave the Artifacts page with it.
                    if this.page == Page::Artifacts {
                        this.read_artifacts(cx);
                    }
                    this.save();
                    cx.notify();
                });
            },
        );
    }

    pub(crate) fn export_session(
        &mut self,
        index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.fill_now(index);
        let c = &self.store.conversations[index];
        let mut text = format!("# {}\n\n", c.title);
        for m in &c.messages {
            let who = if m.notice {
                "Notice".to_string()
            } else if m.role == "user" {
                "You".to_string()
            } else if m.model.is_empty() {
                "Assistant".to_string()
            } else {
                m.model.clone()
            };
            text.push_str(&format!("**{who}**\n\n{}\n\n---\n\n", m.content));
        }
        let name: String = c
            .title
            .chars()
            .map(|ch| {
                if ch.is_alphanumeric() || ch == ' ' {
                    ch
                } else {
                    '-'
                }
            })
            .collect::<String>()
            .trim()
            .to_string();
        let home = std::env::var_os("USERPROFILE")
            .or_else(|| std::env::var_os("HOME"))
            .map(PathBuf::from)
            .unwrap_or_default();
        let receiver = cx.prompt_for_new_path(&home, Some(&format!("{name}.md")));
        cx.spawn_in(window, async move |this, cx| {
            if let Ok(Ok(Some(path))) = receiver.await {
                let failed = std::fs::write(&path, text).is_err();
                let _ = this.update(cx, |this, cx| {
                    if failed {
                        this.error = Some("Could not write the export file.".into());
                    }
                    cx.notify();
                });
            }
        })
        .detach();
    }

    /// Whether session `id` needs you: a helper's call is waiting on you
    /// there, or the turn on screen is and it is this session's.
    pub(crate) fn needs_you(&self, id: u64) -> bool {
        self.agent_asks.iter().any(|request| request.root == id)
            || (self.streaming_id == Some(id)
                && self
                    .turn
                    .as_ref()
                    .is_some_and(|turn| card_is_up(turn, self.asking.as_deref())))
    }

    /// The state the sidebar's filters see the session at `index` in.
    pub(crate) fn session_state(&self, index: usize) -> SessionState {
        let c = &self.store.conversations[index];
        let needs_you = self.needs_you(c.id);
        SessionState {
            needs_you,
            working: !needs_you
                && ((self.busy && self.streaming_id == Some(c.id))
                    || self.helpers_working_in(c.id)),
            unseen: self.unseen.contains(&c.id),
            pinned: c.pinned,
        }
    }

    /// The sidebar's filter, as it was last saved.
    pub(crate) fn session_filter(&self) -> Option<SessionFilter> {
        SessionFilter::parse(&self.store.settings.session_filter)
    }

    /// Pick `filter`, or clear it when it is already the one showing.
    pub(crate) fn toggle_session_filter(&mut self, filter: SessionFilter, cx: &mut Context<Self>) {
        self.store.settings.session_filter = next_filter(self.session_filter(), filter)
            .map_or(String::new(), |f| f.as_str().to_owned());
        self.save();
        cx.notify();
    }

    /// Clear the sidebar's filter, for Escape; true when there was one to
    /// clear, so escape falls through to stopping a turn otherwise.
    pub(crate) fn clear_session_filter(&mut self, cx: &mut Context<Self>) -> bool {
        if self.store.settings.session_filter.is_empty() {
            return false;
        }
        self.store.settings.session_filter.clear();
        self.save();
        cx.notify();
        true
    }

    /// Switch grouping the sidebar by project on or off.
    pub(crate) fn toggle_grouping(&mut self, cx: &mut Context<Self>) {
        self.store.settings.group_by_project ^= true;
        self.save();
        cx.notify();
    }

    /// One button per filter, and a toggle to group by project, in the row
    /// under the sidebar's search box.
    pub(crate) fn session_filter_row(&self, cx: &mut Context<Self>) -> impl IntoElement {
        h_flex()
            .px_2()
            .gap_1()
            .items_center()
            .child(self.filter_chip(SessionFilter::NeedsYou, cx))
            .child(self.filter_chip(SessionFilter::Working, cx))
            .child(self.filter_chip(SessionFilter::Unseen, cx))
            .child(self.filter_chip(SessionFilter::Pinned, cx))
            .child(div().flex_1())
            .child(self.group_toggle(cx))
    }

    fn filter_chip(&self, filter: SessionFilter, cx: &mut Context<Self>) -> impl IntoElement {
        let on = self.session_filter() == Some(filter);
        Button::new(("session-filter", filter as usize))
            .plain(cx)
            .xsmall()
            .selected(on)
            .icon(Icon::new(filter.icon()).size_3().text_color(if on {
                cx.theme().primary
            } else {
                cx.theme().muted_foreground
            }))
            .tooltip(filter.label())
            .on_click(cx.listener(move |this, _, _, cx| this.toggle_session_filter(filter, cx)))
    }

    fn group_toggle(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let on = self.store.settings.group_by_project;
        Button::new("group-by-project")
            .plain(cx)
            .xsmall()
            .selected(on)
            .icon(Icon::new(AppIcon::Folder).size_3().text_color(if on {
                cx.theme().primary
            } else {
                cx.theme().muted_foreground
            }))
            .tooltip("Group by project")
            .on_click(cx.listener(|this, _, _, cx| this.toggle_grouping(cx)))
    }
}

#[cfg(test)]
mod tests {
    use super::{
        Conversation, SessionFilter, SessionState, group_by_project, next_filter, passes,
        project_label,
    };
    use std::path::PathBuf;

    fn state(needs_you: bool, working: bool, unseen: bool, pinned: bool) -> SessionState {
        SessionState {
            needs_you,
            working,
            unseen,
            pinned,
        }
    }

    #[test]
    fn each_filter_passes_only_its_own_state() {
        let waiting = state(true, false, false, false);
        let running = state(false, true, false, false);
        let woken = state(false, false, true, false);
        let kept = state(false, false, false, true);
        assert!(passes(waiting, SessionFilter::NeedsYou));
        assert!(!passes(waiting, SessionFilter::Working));
        assert!(passes(running, SessionFilter::Working));
        assert!(!passes(running, SessionFilter::Unseen));
        assert!(passes(woken, SessionFilter::Unseen));
        assert!(!passes(woken, SessionFilter::Pinned));
        assert!(passes(kept, SessionFilter::Pinned));
        assert!(!passes(kept, SessionFilter::NeedsYou));
    }

    #[test]
    fn picking_a_filter_again_clears_it() {
        assert_eq!(
            next_filter(None, SessionFilter::Working),
            Some(SessionFilter::Working)
        );
        assert_eq!(
            next_filter(Some(SessionFilter::Working), SessionFilter::Working),
            None
        );
        assert_eq!(
            next_filter(Some(SessionFilter::Working), SessionFilter::Pinned),
            Some(SessionFilter::Pinned)
        );
    }

    fn with_workspace(workspace: Option<&str>) -> Conversation {
        Conversation {
            workspace: workspace.map(PathBuf::from),
            ..Default::default()
        }
    }

    #[test]
    fn sessions_group_by_project_keeping_their_order_and_no_folder_together() {
        let conversations = vec![
            with_workspace(Some("/code/letronna")),
            with_workspace(None),
            with_workspace(Some("/code/other")),
            with_workspace(Some("/code/letronna")),
            with_workspace(None),
        ];
        let groups = group_by_project(&conversations, &[0, 1, 2, 3, 4]);
        assert_eq!(
            groups,
            vec![
                (Some(PathBuf::from("/code/letronna")), vec![0, 3]),
                (None, vec![1, 4]),
                (Some(PathBuf::from("/code/other")), vec![2]),
            ]
        );
    }

    #[test]
    fn a_project_group_is_named_for_its_folder_or_has_no_project() {
        assert_eq!(
            project_label(Some(&PathBuf::from("/code/letronna"))),
            "letronna"
        );
        assert_eq!(project_label(None), "No project");
    }
}
