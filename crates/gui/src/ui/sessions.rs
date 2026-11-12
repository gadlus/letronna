use crate::app::card_is_up;
use crate::prelude::*;
use gpui_component::{Selectable, menu::PopupMenu, notification::Notification};

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
        DeleteSession,
        ToggleSelected,
        SelectPrevious,
        SelectNext,
        SelectAll,
        ClearSelection
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

/// How long a press on a session's row is held still before its menu
/// opens: the long press of a trackpad or a touch screen, which gpui gives
/// a mouse no event for.
const LONG_PRESS: std::time::Duration = std::time::Duration::from_millis(500);

/// How far, in pixels, a held press may wander and still open the menu,
/// so a hand that shakes a little is not taken for a drag.
const PRESS_SLOP: f64 = 8.;

/// A session's menu in the sidebar, while it is open. It names the session
/// by id: a row's place moves when a session before it is deleted or a new
/// one is started, and the menu must act on the one it was opened on.
pub(crate) struct SessionMenu {
    pub(crate) id: u64,
    /// Where it opened, in the window.
    pub(crate) at: Point<Pixels>,
    pub(crate) menu: Entity<PopupMenu>,
    /// What had the keys before it, which Escape gives them back to.
    back: Option<FocusHandle>,
    _closes: [Subscription; 2],
}

/// What fills a session's menu, or the selection's, as it opens.
type MenuItems = Box<dyn FnOnce(PopupMenu, &mut Window, &mut Context<PopupMenu>) -> PopupMenu>;

/// A left press on a session's row, which opens its menu once held.
pub(crate) struct Press {
    id: u64,
    at: Point<Pixels>,
    /// Which press it is, so the wait of an earlier one opens nothing.
    since: std::time::Instant,
    /// The menu has opened from it, so letting go is not a click as well.
    held: bool,
}

/// Whether `to` is still close enough to `from` for a press to be held.
fn still(from: Point<Pixels>, to: Point<Pixels>) -> bool {
    (to - from).magnitude() <= PRESS_SLOP
}

/// Why session `id` cannot be deleted yet, if it cannot. Only what runs in
/// that session holds it; a reply in another one does not, which once kept
/// every session from being deleted while any of them answered.
pub(crate) fn delete_refusal(
    id: u64,
    replying_in: Option<u64>,
    summarizing: bool,
    running: bool,
) -> Option<&'static str> {
    if replying_in == Some(id) {
        // Its reply would go on writing into a session no longer there.
        Some("Stop the reply in this session before deleting it.")
    } else if summarizing {
        // Its id can be handed out again before the summary lands.
        Some("Wait for the summary before deleting the session.")
    } else if running {
        // Its end would come for a session no longer there.
        Some("Stop its run before deleting this session.")
    } else {
        None
    }
}

/// The session listed after the one at `index` in `listed`, or before it
/// when it is the last, for the sidebar's keys to go on to once it is
/// deleted.
fn beside(listed: &[usize], index: usize) -> Option<usize> {
    let at = listed.iter().position(|&i| i == index)?;
    let next = listed
        .get(at + 1)
        .or(at.checked_sub(1).and_then(|up| listed.get(up)));
    next.copied()
}

/// The session to show once the one at `deleted` is gone: the one that was
/// on screen, `on_screen`, found again by id as the places after it moved
/// up, or if that was the one deleted, the one listed under it.
fn active_after(store: &Store, on_screen: u64, deleted: usize) -> usize {
    match store.index_of(on_screen) {
        Some(still) => still,
        None => deleted.saturating_sub(1),
    }
}

/// `selected` with `id` added at the end, or taken out when it was in.
fn toggled(mut selected: Vec<u64>, id: u64) -> Vec<u64> {
    match selected.iter().position(|&s| s == id) {
        Some(at) => {
            selected.remove(at);
        }
        None => selected.push(id),
    }
    selected
}

/// The sessions `listed` from `from` to `to`, both included, in the order
/// listed: what a Shift-click selects. From a session no longer listed,
/// `to` alone.
pub(crate) fn span(listed: &[u64], from: u64, to: u64) -> Vec<u64> {
    let at = |id: u64| listed.iter().position(|&listed| listed == id);
    let (Some(from), Some(to_at)) = (at(from), at(to)) else {
        return vec![to];
    };
    listed[from.min(to_at)..=from.max(to_at)].to_vec()
}

/// Whether one Pin for sessions pinned or not, as `pinned` says, pins them
/// all: unless every one already is, when it unpins them.
fn pins(pinned: impl IntoIterator<Item = bool>) -> bool {
    pinned.into_iter().any(|pinned| !pinned)
}

/// Sessions by name, for a sentence: all of them while there are three or
/// fewer, else the first two and how many more.
fn named(titles: &[String]) -> String {
    let quoted = |title: &String| format!("\"{title}\"");
    match titles {
        [] => String::new(),
        [one] => quoted(one),
        [first, second, rest @ ..] if rest.len() > 1 => {
            format!(
                "{}, {} and {} more",
                quoted(first),
                quoted(second),
                rest.len()
            )
        }
        [most @ .., last] => {
            let most: Vec<String> = most.iter().map(quoted).collect();
            format!("{} and {}", most.join(", "), quoted(last))
        }
    }
}

/// What the dialog says before the sessions `titles` are deleted, and of
/// the `held` ones selected with them that something still runs in.
fn deleting_several(titles: &[String], held: usize) -> String {
    let mut said = match titles {
        [one] => format!("\"{one}\" and its messages will be removed from this device."),
        _ => format!(
            "{} sessions and their messages will be removed from this device: {}.",
            titles.len(),
            named(titles)
        ),
    };
    match held {
        0 => {}
        1 => said.push_str(" 1 more has something running in it and stays."),
        n => said.push_str(&format!(
            " {n} more have something running in them and stay."
        )),
    }
    said
}

/// A session as Markdown: its title, then each message under who said it.
fn markdown_of(c: &Conversation) -> String {
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
    text
}

/// The file name a session called `title` is exported under, less its
/// `.md`: letters, digits and spaces kept, anything else a dash.
fn export_name(title: &str) -> String {
    title
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
        .to_string()
}

/// Write each of `files`, a name and its Markdown, into `folder`, numbering
/// a name already taken there rather than writing over it. How many could
/// not be written.
fn write_exports(folder: &std::path::Path, files: &[(String, String)]) -> usize {
    let mut failed = 0;
    for (name, text) in files {
        let name = if name.is_empty() { "Session" } else { name };
        let free = (1..1000)
            .map(|n| match n {
                1 => folder.join(format!("{name}.md")),
                n => folder.join(format!("{name} {n}.md")),
            })
            .find(|path| !path.exists());
        let Some(path) = free else {
            failed += 1;
            continue;
        };
        if std::fs::write(&path, text).is_err() {
            failed += 1;
        }
    }
    failed
}

impl Chat {
    /// What session `id`'s menu holds. Each item finds the session again
    /// when it is picked, as the menu may have been open while it moved.
    fn session_menu_items(
        &self,
        id: u64,
        cx: &mut Context<Self>,
    ) -> impl FnOnce(PopupMenu, &mut Window, &mut Context<PopupMenu>) -> PopupMenu + 'static {
        let chat = cx.entity().downgrade();
        let pinned =
            (self.store.index_of(id)).is_some_and(|ix| self.store.conversations[ix].pinned);
        // To act on several at once, found without knowing Cmd-click.
        let select_label = if self.selected.contains(&id) {
            "Deselect"
        } else if self.selecting() {
            "Add to selection"
        } else {
            "Select"
        };
        move |menu, _, _| {
            let item =
                |label: &'static str,
                 icon: AppIcon,
                 f: fn(&mut Chat, usize, &mut Window, &mut Context<Chat>)| {
                    let chat = chat.clone();
                    PopupMenuItem::new(label).icon(Icon::new(icon)).on_click(
                        move |_, window, cx| {
                            let _ = chat.update(cx, |this, cx| {
                                if let Some(index) = this.store.index_of(id) {
                                    f(this, index, window, cx);
                                }
                            });
                        },
                    )
                };
            menu.min_w(px(200.))
                .item(item("Rename", AppIcon::Pencil, Chat::rename_session))
                .item(item("Name it again", AppIcon::Sparkles, Chat::name_again))
                .item(item(
                    if pinned { "Unpin" } else { "Pin" },
                    AppIcon::Pin,
                    Chat::toggle_pin,
                ))
                .item(item(
                    select_label,
                    AppIcon::Checklist,
                    Chat::toggle_selected_at,
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

    /// Open session `id`'s menu at `at` in the window, in place of any menu
    /// already open, with the keys in it.
    pub(crate) fn open_session_menu(
        &mut self,
        id: u64,
        at: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.store.index_of(id).is_none() {
            return;
        }
        // From one menu straight to another, the keys go back to where
        // they were before the first.
        let back = match &self.session_menu {
            Some(open) if open.menu.focus_handle(cx).contains_focused(window, cx) => {
                open.back.clone()
            }
            _ => window.focused(cx),
        };
        // A selected row's menu is the selection's, as in a file list.
        let build: MenuItems = if self.selected.len() > 1 && self.selected.contains(&id) {
            Box::new(self.selection_menu_items(cx))
        } else {
            Box::new(self.session_menu_items(id, cx))
        };
        let menu = PopupMenu::build(window, cx, build);
        menu.focus_handle(cx).focus(window, cx);
        let dismissed = cx.subscribe_in(
            &menu,
            window,
            move |this, menu, _: &DismissEvent, window, cx| {
                // Escape gives the keys back; an item that moved them, to a
                // dialog, leaves them there.
                let back = (this.session_menu.as_ref())
                    .filter(|open| open.menu == *menu)
                    .and_then(|open| open.back.clone());
                if let Some(back) = back
                    && menu.focus_handle(cx).contains_focused(window, cx)
                {
                    window.focus(&back, cx);
                }
                this.close_session_menu(menu.entity_id(), cx);
            },
        );
        // It closes only on Escape, a click outside or an item otherwise,
        // and F6 or Ctrl+L would leave it hung open over the sidebar.
        let shown = menu.entity_id();
        let left = cx.on_focus_out(&menu.focus_handle(cx), window, move |this, _, _, cx| {
            this.close_session_menu(shown, cx);
        });
        self.session_menu = Some(SessionMenu {
            id,
            at,
            menu,
            back,
            _closes: [dismissed, left],
        });
        cx.notify();
    }

    /// Close the menu `shown`, unless another has opened in its place: a
    /// right-click on the next row closes the first menu as it opens its own.
    fn close_session_menu(&mut self, shown: EntityId, cx: &mut Context<Self>) {
        if (self.session_menu.as_ref()).is_some_and(|open| open.menu.entity_id() == shown) {
            self.session_menu = None;
            cx.notify();
        }
    }

    /// A left press on session `id`'s row at `at` began. Held there, it
    /// opens the row's menu, as a long press does on a phone.
    pub(crate) fn press_session(
        &mut self,
        id: u64,
        at: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let since = std::time::Instant::now();
        self.session_press = Some(Press {
            id,
            at,
            since,
            held: false,
        });
        // Left to run out rather than kept: a press let go of before then
        // is simply no longer the one waiting.
        cx.spawn_in(window, async move |this, cx| {
            cx.background_executor().timer(LONG_PRESS).await;
            let _ = this.update_in(cx, |this, window, cx| {
                let Some(press) = this.session_press.as_mut() else {
                    return;
                };
                if press.since != since || press.held {
                    return;
                }
                // Moved away without a move the row saw, off its edge.
                if !still(press.at, window.mouse_position()) {
                    this.session_press = None;
                    return;
                }
                press.held = true;
                this.open_session_menu(id, at, window, cx);
            });
        })
        .detach();
    }

    /// The pointer moved to `to` over session `id`'s row: a press that
    /// wanders is a drag or a slip, and opens nothing.
    pub(crate) fn move_press(&mut self, id: u64, to: Point<Pixels>) {
        if (self.session_press.as_ref())
            .is_some_and(|press| press.id == id && !press.held && !still(press.at, to))
        {
            self.session_press = None;
        }
    }

    /// A press on session `id`'s row was let go of: before the menu opened,
    /// it is a click and nothing more. Over the row, a held one is kept
    /// for the click that follows, which `held_press` then takes.
    pub(crate) fn let_go_press(&mut self, id: u64, over: bool) {
        if (self.session_press.as_ref())
            .is_some_and(|press| press.id == id && (!press.held || !over))
        {
            self.session_press = None;
        }
    }

    /// Whether the click on a row ends a press that opened its menu, so
    /// the click does not also open the session.
    pub(crate) fn held_press(&mut self) -> bool {
        self.session_press.take().is_some_and(|press| press.held)
    }

    /// Have a model name session `id` once its first reply is in, when
    /// Settings say so and it is still called by its first line.
    pub(crate) fn name_after_first_reply(&mut self, id: u64, cx: &mut Context<Self>) {
        if !self.store.settings.auto_title {
            return;
        }
        let Some(ix) = self.store.index_of(id) else {
            return;
        };
        let conversation = &self.store.conversations[ix];
        let mut asked = (conversation.messages.iter())
            .filter(|m| letronna_core::chat::opens_turn(m) && !m.notice);
        let (Some(first), None) = (asked.next(), asked.next()) else {
            return;
        };
        let typed = letronna_core::references::typed(&first.content);
        if conversation.title != crate::app::first_line(typed) {
            return;
        }
        self.name_session(id, false, cx);
    }

    /// Have a model name the session at `index` now, from what was said in
    /// it, as Rename's neighbour in its menu.
    pub(crate) fn name_again(&mut self, index: usize, _: &mut Window, cx: &mut Context<Self>) {
        // A session only listed has its messages on the disk.
        self.fill_now(index);
        let id = self.store.conversations[index].id;
        self.name_session(id, true, cx);
    }

    /// Ask a model for session `id`'s name off the UI thread, and give it
    /// the name only if nobody renamed it meanwhile; `said` when the user
    /// asked, who hears how it went.
    fn name_session(&mut self, id: u64, said: bool, cx: &mut Context<Self>) {
        let Some(ix) = self.store.index_of(id) else {
            return;
        };
        let connection = match self.connection_for(ix, None) {
            Ok(connection) => connection,
            Err(why) => {
                if said {
                    self.note = Some(why);
                    cx.notify();
                }
                return;
            }
        };
        let conversation = &self.store.conversations[ix];
        let (was, messages) = (conversation.title.clone(), conversation.messages.clone());
        let named = cx
            .background_executor()
            .spawn(async move { letronna_core::title::name(&connection, &messages) });
        cx.spawn(async move |this, cx| {
            let named = named.await;
            let _ = this.update(cx, |this, cx| {
                let Some(ix) = this.store.index_of(id) else {
                    return;
                };
                match named {
                    Ok(name) if this.store.conversations[ix].title == was => {
                        this.store.conversations[ix].title = name;
                        this.save();
                    }
                    Ok(_) => {}
                    Err(why) if said => this.note = Some(why),
                    Err(_) => {}
                }
                cx.notify();
            });
        })
        .detach();
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
        let id = self.store.conversations[index].id;
        let title = self.store.conversations[index].title.clone();
        self.rename
            .update(cx, |input, cx| input.set_value(title, window, cx));
        let input = self.rename.clone();
        let chat = cx.entity();
        window.open_alert_dialog(cx, move |dialog, _, _| {
            let input = input.clone();
            let chat = chat.clone();
            dialog
                .title(div().font_family(HEADING_FONT).child("Rename session"))
                .width(px(420.))
                .confirm()
                .child(Input::new(&input))
                .on_ok(move |_, _, cx| {
                    let title = input.read(cx).value().trim().to_string();
                    chat.update(cx, |this, cx| {
                        // Found again: it may have moved while the dialog was up.
                        if let Some(index) = this.store.index_of(id)
                            && !title.is_empty()
                        {
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

    /// Why session `id` cannot be deleted yet, if it cannot.
    pub(crate) fn why_not_delete(&self, id: u64) -> Option<&'static str> {
        delete_refusal(
            id,
            self.streaming_id.filter(|_| self.busy),
            self.compacting.contains(&id),
            self.runs.contains_key(&id),
        )
    }

    /// Ask, then delete the session at `index`. Said in a toast when it
    /// cannot be, as the chat it would be said in may not be on screen.
    pub(crate) fn delete_session(
        &mut self,
        index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let id = self.store.conversations[index].id;
        if let Some(why) = self.why_not_delete(id) {
            window.push_notification(Notification::warning(why), cx);
            return;
        }
        let title = self.store.conversations[index].shown_title();
        let chat = cx.entity();
        super::confirm_delete(
            "Delete session",
            format!("\"{title}\" and its messages will be removed from this device."),
            window,
            cx,
            move |window, cx| {
                chat.update(cx, |this, cx| this.delete_now(id, window, cx));
            },
        );
    }

    /// Delete session `id` now that the user said so. Found again by id: a
    /// session deleted or started while the dialog was up moves the others.
    fn delete_now(&mut self, id: u64, window: &mut Window, cx: &mut Context<Self>) {
        let Some(index) = self.store.index_of(id) else {
            return;
        };
        // A reply may have begun there while the dialog was up.
        if let Some(why) = self.why_not_delete(id) {
            window.push_notification(Notification::warning(why), cx);
            return;
        }
        // The sidebar's keys go on to the session next in its list, so
        // Delete can be pressed again for it.
        let next = (self.picked.filter(|&picked| picked == id))
            .and_then(|_| beside(&self.listed_sessions(cx), index))
            .map(|i| self.store.conversations[i].id);
        let on_screen = self.store.conversations[self.store.active].id;
        self.stop_review(id);
        letronna_core::browser::forget(id);
        self.drafts.remove(&id);
        self.open.retain(|&open| open != id);
        self.recent.retain(|&recent| recent != id);
        self.panel.forget(id);
        // The next new session can be given this id again, and must not
        // start out with what was allowed here, nor in this one's mode.
        self.session_allow.remove(&id);
        letronna_core::approvals::forget_session(id);
        // Nor with this one's loop, whose ticks would go there.
        letronna_core::wake::stop(id);
        self.modes.remove(&id);
        // Nor with this one's rows as they were: what was open, how long a
        // reply thought, what an edit changed.
        self.thinking_open.retain(|&(session, _)| session != id);
        self.runs_open.retain(|&(session, _)| session != id);
        self.previews_open.retain(|&(session, _)| session != id);
        self.thought_for.retain(|&(session, _), _| session != id);
        self.edited.retain(|&(session, _), _| session != id);
        self.plan_folded.remove(&id);
        // Nor show its helpers' cards; dropped, they are declined.
        self.agent_asks.retain(|request| request.root != id);
        // Nor the pictures waiting to go with its next message, Auto mode's
        // tags or whether it was woken while away.
        self.attachments.remove(&id);
        self.tags.remove(&id);
        self.unseen.remove(&id);
        self.kept_own.remove(&id);
        if self.session_menu.as_ref().is_some_and(|open| open.id == id) {
            self.session_menu = None;
        }
        self.selected.retain(|&selected| selected != id);
        if self.select_anchor == Some(id) {
            self.select_anchor = None;
        }
        if self
            .session_press
            .as_ref()
            .is_some_and(|press| press.id == id)
        {
            self.session_press = None;
        }
        // Through the store, so the session's file goes too, and what is
        // kept beside it.
        self.store.remove(index);
        if self.store.conversations.is_empty() {
            self.picked = None;
            self.new_chat(window, cx);
            return;
        }
        if next.is_some() {
            self.picked = next;
        }
        self.store.active = active_after(&self.store, on_screen, index);
        if on_screen == id {
            // Its typing goes with it, and the next one's comes back.
            self.swap_draft(window, cx);
            self.follow();
        }
        // Its files leave the Artifacts page with it.
        if self.page == Page::Artifacts {
            self.read_artifacts(cx);
        }
        self.save();
        cx.notify();
    }

    /// Whether sessions are selected in the sidebar, to act on together.
    pub(crate) fn selecting(&self) -> bool {
        !self.selected.is_empty()
    }

    /// The sessions selected that are still here, by index, in the order
    /// they were picked.
    fn selected_indices(&self) -> Vec<usize> {
        (self.selected.iter())
            .filter_map(|&id| self.store.index_of(id))
            .collect()
    }

    /// The sessions the sidebar lists, by id, in its order.
    fn listed_ids(&self, cx: &App) -> Vec<u64> {
        (self.listed_sessions(cx).into_iter())
            .map(|index| self.store.conversations[index].id)
            .collect()
    }

    /// Select session `id` as well, or no longer, as Cmd-click does on a
    /// Mac and Ctrl-click elsewhere.
    pub(crate) fn toggle_selected(&mut self, id: u64, window: &mut Window, cx: &mut Context<Self>) {
        self.selected = toggled(std::mem::take(&mut self.selected), id);
        self.select_anchor = Some(id);
        self.selection_moved(id, window, cx);
    }

    /// Select the session at `index` as well, or no longer: Space in the
    /// sidebar, and Select in a session's menu.
    pub(crate) fn toggle_selected_at(
        &mut self,
        index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let id = self.store.conversations[index].id;
        self.toggle_selected(id, window, cx);
    }

    /// Select every session listed from the last one clicked, or else the
    /// one on screen, to `id`, as Shift-click does.
    pub(crate) fn select_span(&mut self, id: u64, window: &mut Window, cx: &mut Context<Self>) {
        let listed = self.listed_ids(cx);
        let on_screen = self.store.conversations[self.store.active].id;
        let from = [self.select_anchor, Some(on_screen)]
            .into_iter()
            .flatten()
            .find(|from| listed.contains(from))
            .unwrap_or(id);
        self.selected = span(&listed, from, id);
        self.select_anchor = Some(from);
        self.selection_moved(id, window, cx);
    }

    /// Select every session the sidebar lists.
    pub(crate) fn select_all(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.selected = self.listed_ids(cx);
        let Some(&first) = self.selected.first() else {
            return;
        };
        let picked = self.picked.unwrap_or(first);
        self.selection_moved(picked, window, cx);
    }

    /// The selection changed at session `id`. While there is one, the
    /// sidebar has the keys, on `id`, so Delete, P and Escape act on it from
    /// there; once there is none, see `selection_ended`.
    fn selection_moved(&mut self, id: u64, window: &mut Window, cx: &mut Context<Self>) {
        if self.selecting() {
            self.picked = Some(id);
            window.focus(&self.sidebar_focus, cx);
        } else {
            self.selection_ended(window, cx);
        }
        cx.notify();
    }

    /// Select nothing. True when something was, so Escape goes on to what
    /// else it closes otherwise.
    pub(crate) fn clear_selection(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if !self.selecting() {
            return false;
        }
        self.selected.clear();
        self.select_anchor = None;
        self.selection_ended(window, cx);
        cx.notify();
        true
    }

    /// Nothing is selected any more: the keys a selection gave the sidebar
    /// go back to the box, where a letter is typed rather than acting on a
    /// session. Those F6 gave it stay, as F6 is how they leave.
    fn selection_ended(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.sidebar_focus.is_focused(window)
            && self.region != Some(crate::ui::keys::Region::Sidebar)
        {
            self.focus_composer(window, cx);
        }
    }

    /// Pin the sessions selected, or unpin them when every one already is.
    pub(crate) fn pin_selected(&mut self, cx: &mut Context<Self>) {
        let indices = self.selected_indices();
        let pin = pins(indices.iter().map(|&i| self.store.conversations[i].pinned));
        for index in indices {
            self.store.conversations[index].pinned = pin;
        }
        self.save();
        cx.notify();
    }

    /// The sidebar's P: the selection when there is one, else the session
    /// at `index`.
    pub(crate) fn pin_picked(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        if self.selecting() {
            self.pin_selected(cx);
        } else {
            self.toggle_pin(index, window, cx);
        }
    }

    /// The sidebar's Delete: the selection when there is one, else the
    /// session at `index`.
    pub(crate) fn delete_picked(
        &mut self,
        index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.selecting() {
            self.delete_selected(window, cx);
        } else {
            self.delete_session(index, window, cx);
        }
    }

    /// Have a model name each session selected again.
    fn name_selected_again(&mut self, cx: &mut Context<Self>) {
        for index in self.selected_indices() {
            self.fill_now(index);
            let id = self.store.conversations[index].id;
            self.name_session(id, true, cx);
        }
    }

    /// Ask, then delete the sessions selected. Those something still runs
    /// in are left selected, and the dialog says so; with only those, a
    /// toast says why instead.
    pub(crate) fn delete_selected(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let ids: Vec<u64> = (self.selected_indices().into_iter())
            .map(|index| self.store.conversations[index].id)
            .collect();
        let (free, held): (Vec<u64>, Vec<u64>) = ids
            .into_iter()
            .partition(|&id| self.why_not_delete(id).is_none());
        if free.is_empty() {
            if let Some(why) = held.first().and_then(|&id| self.why_not_delete(id)) {
                window.push_notification(Notification::warning(why), cx);
            }
            return;
        }
        let titles: Vec<String> = (free.iter())
            .filter_map(|&id| self.store.index_of(id))
            .map(|index| self.store.conversations[index].shown_title())
            .collect();
        let chat = cx.entity();
        super::confirm_delete(
            "Delete sessions",
            deleting_several(&titles, held.len()),
            window,
            cx,
            move |window, cx| {
                chat.update(cx, |this, cx| {
                    for &id in &free {
                        this.delete_now(id, window, cx);
                    }
                    if !this.selecting() {
                        this.selection_ended(window, cx);
                    }
                    cx.notify();
                });
            },
        );
    }

    /// Write each session selected to a Markdown file in a folder the user
    /// picks.
    fn export_selected(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let mut files = Vec::new();
        for index in self.selected_indices() {
            // A session only listed has its messages on the disk.
            self.fill_now(index);
            let c = &self.store.conversations[index];
            files.push((export_name(&c.title), markdown_of(c)));
        }
        let receiver = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("Export here".into()),
        });
        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(picked))) = receiver.await else {
                return;
            };
            let Some(folder) = picked.into_iter().next() else {
                return;
            };
            let written = files.len();
            let shown = folder.display().to_string();
            let failed = cx
                .background_executor()
                .spawn(async move { write_exports(&folder, &files) })
                .await;
            let _ = this.update(cx, |this, cx| {
                if failed > 0 {
                    this.error = Some(format!(
                        "Could not write {failed} of the {written} export files."
                    ));
                } else {
                    this.note = Some(format!("Exported {written} sessions to {shown}."));
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// What the menu of the sessions selected holds, opened from the
    /// selection bar or from a selected row.
    pub(crate) fn selection_menu_items(
        &self,
        cx: &mut Context<Self>,
    ) -> impl Fn(PopupMenu, &mut Window, &mut Context<PopupMenu>) -> PopupMenu + 'static {
        let chat = cx.entity().downgrade();
        let indices = self.selected_indices();
        let count = indices.len();
        let pin = pins(indices.iter().map(|&i| self.store.conversations[i].pinned));
        let all = (self.listed_ids(cx).iter()).all(|id| self.selected.contains(id));
        move |menu, _, _| {
            let item = |label: SharedString,
                        icon: Icon,
                        f: fn(&mut Chat, &mut Window, &mut Context<Chat>)| {
                let chat = chat.clone();
                PopupMenuItem::new(label)
                    .icon(icon)
                    .on_click(move |_, window, cx| {
                        let _ = chat.update(cx, |this, cx| f(this, window, cx));
                    })
            };
            let menu = menu
                .min_w(px(200.))
                .label(format!("{count} selected"))
                .item(item(
                    if pin { "Pin" } else { "Unpin" }.into(),
                    Icon::new(AppIcon::Pin),
                    |this, _, cx| this.pin_selected(cx),
                ))
                .item(item(
                    "Name them again".into(),
                    Icon::new(AppIcon::Sparkles),
                    |this, _, cx| this.name_selected_again(cx),
                ))
                .item(item(
                    "Export as Markdown".into(),
                    Icon::new(AppIcon::Export),
                    Chat::export_selected,
                ))
                .separator();
            let menu = if all {
                menu
            } else {
                menu.item(item(
                    "Select all".into(),
                    Icon::new(AppIcon::Checklist),
                    Chat::select_all,
                ))
            };
            menu.item(item(
                "Clear selection".into(),
                Icon::new(IconName::Close),
                |this, window, cx| {
                    this.clear_selection(window, cx);
                },
            ))
            .separator()
            .item(item(
                format!("Delete {count} sessions").into(),
                Icon::new(AppIcon::Trash),
                Chat::delete_selected,
            ))
        }
    }

    /// In place of the filters while sessions are selected: how many, and
    /// what can be done to them all at once.
    pub(crate) fn selection_bar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let indices = self.selected_indices();
        let pin = pins(indices.iter().map(|&i| self.store.conversations[i].pinned));
        let menu = self.selection_menu_items(cx);
        let muted = cx.theme().muted_foreground;
        h_flex()
            .px_2()
            .gap_1()
            .items_center()
            .child(
                Button::new("selection-done")
                    .plain(cx)
                    .xsmall()
                    .icon(Icon::new(IconName::Close).size_3().text_color(muted))
                    .tooltip("Done selecting")
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.clear_selection(window, cx);
                    })),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_xs()
                    .font_family(HEADING_FONT)
                    .font_weight(FontWeight::MEDIUM)
                    .child(format!("{} selected", indices.len())),
            )
            .child(
                Button::new("selection-pin")
                    .plain(cx)
                    .xsmall()
                    .icon(Icon::new(AppIcon::Pin).size_3().text_color(muted))
                    .tooltip(if pin { "Pin" } else { "Unpin" })
                    .on_click(cx.listener(|this, _, _, cx| this.pin_selected(cx))),
            )
            .child(
                Button::new("selection-delete")
                    .plain(cx)
                    .xsmall()
                    .icon(Icon::new(AppIcon::Trash).size_3().text_color(muted))
                    .tooltip("Delete")
                    .on_click(cx.listener(|this, _, window, cx| this.delete_selected(window, cx))),
            )
            .child(
                Button::new("selection-more")
                    .plain(cx)
                    .xsmall()
                    .icon(Icon::new(IconName::Ellipsis).size_3().text_color(muted))
                    .tooltip("More")
                    .dropdown_menu(menu),
            )
    }

    pub(crate) fn export_session(
        &mut self,
        index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.fill_now(index);
        let c = &self.store.conversations[index];
        let text = markdown_of(c);
        let name = export_name(&c.title);
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

    /// How many of the sessions the sidebar could list are in each filter's
    /// state, in the order of `SessionFilter::ALL`.
    fn session_counts(&self) -> [usize; 4] {
        let mut counts = [0; 4];
        for (index, conversation) in self.store.conversations.iter().enumerate() {
            // A new session is listed once something is said in it.
            if untouched(conversation) {
                continue;
            }
            let state = self.session_state(index);
            for filter in SessionFilter::ALL {
                if passes(state, filter) {
                    counts[filter as usize] += 1;
                }
            }
        }
        counts
    }

    /// Which sessions the sidebar lists, in words, in the row under its
    /// search box: one menu whose button names what is shown, in the colour
    /// of a link while some sessions are hidden, and lists every filter with
    /// how many sessions it holds, and grouping by project. Four glyphs
    /// stood here before, and nothing said which was which.
    ///
    /// Beside the menu, only while there are any, the sessions that wait for
    /// you and those with something unseen: a mark and a count each, which
    /// shows just those when pressed and all of them when pressed again.
    pub(crate) fn session_filter_row(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let picked = self.session_filter();
        let counts = self.session_counts();
        let grouped = self.store.settings.group_by_project;
        let chat = cx.entity();
        let theme = cx.theme();
        let (muted, link, amber) = (theme.muted_foreground, theme.link, theme.warning);
        let menu = Button::new("session-filter")
            .plain(cx)
            .xsmall()
            .tooltip("Which sessions are listed")
            .child(
                h_flex()
                    .gap_1()
                    .items_center()
                    .text_xs()
                    .text_color(if picked.is_some() { link } else { muted })
                    .child(Icon::new(AppIcon::Filter).size_3())
                    .child(picked.map_or("All sessions", SessionFilter::label))
                    .child(Icon::new(IconName::ChevronDown).size_3()),
            )
            .dropdown_menu(move |menu, _, _| {
                let all = chat.clone();
                let mut menu = menu.min_w(px(190.)).item(
                    PopupMenuItem::new("All sessions")
                        .checked(picked.is_none())
                        .on_click(move |_, _, cx| {
                            all.update(cx, |this, cx| {
                                this.clear_session_filter(cx);
                            })
                        }),
                );
                for filter in SessionFilter::ALL {
                    let chat = chat.clone();
                    menu = menu.item(
                        PopupMenuItem::new(menu_label(filter, counts[filter as usize]))
                            .checked(picked == Some(filter))
                            .on_click(move |_, _, cx| {
                                chat.update(cx, |this, cx| this.toggle_session_filter(filter, cx))
                            }),
                    );
                }
                let chat = chat.clone();
                menu.separator().item(
                    PopupMenuItem::new("Group by project")
                        .checked(grouped)
                        .on_click(move |_, _, cx| {
                            chat.update(cx, |this, cx| this.toggle_grouping(cx))
                        }),
                )
            });
        // The same marks the session rows and the page rows use: the
        // triangle for what waits, the dot for what has not been seen.
        let waiting = [
            (
                SessionFilter::NeedsYou,
                Icon::new(AppIcon::Alert)
                    .size_3()
                    .text_color(amber)
                    .into_any_element(),
            ),
            (
                SessionFilter::Unseen,
                div()
                    .size(px(6.))
                    .rounded_full()
                    .bg(link)
                    .into_any_element(),
            ),
        ]
        .into_iter()
        .filter(|(filter, _)| counts[*filter as usize] > 0)
        .map(|(filter, mark)| {
            let count = counts[filter as usize];
            Button::new(("session-waiting", filter as usize))
                .plain(cx)
                .xsmall()
                .selected(picked == Some(filter))
                .tooltip(waiting_tip(filter, count))
                .child(
                    h_flex()
                        .gap_1()
                        .items_center()
                        .text_xs()
                        .child(mark)
                        .child(count.to_string()),
                )
                .on_click(cx.listener(move |this, _, _, cx| this.toggle_session_filter(filter, cx)))
        })
        .collect::<Vec<_>>();
        h_flex()
            .px_1()
            .gap_1()
            .items_center()
            .child(menu)
            .child(div().flex_1())
            .children(waiting)
    }
}

/// A filter's line in the menu: its name, and how many sessions it holds
/// when it holds any.
fn menu_label(filter: SessionFilter, count: usize) -> String {
    match count {
        0 => filter.label().to_owned(),
        count => format!("{} · {count}", filter.label()),
    }
}

/// What a count beside the menu says under the pointer.
fn waiting_tip(filter: SessionFilter, count: usize) -> String {
    let (one, many) = match filter {
        SessionFilter::NeedsYou => ("session needs you", "sessions need you"),
        SessionFilter::Working => ("session is working", "sessions are working"),
        SessionFilter::Unseen => (
            "session has something you have not seen",
            "sessions have something you have not seen",
        ),
        SessionFilter::Pinned => ("session is pinned", "sessions are pinned"),
    };
    match count {
        1 => format!("1 {one}"),
        count => format!("{count} {many}"),
    }
}

#[cfg(test)]
mod tests {
    use super::{
        Conversation, SessionFilter, SessionState, Store, active_after, beside, delete_refusal,
        deleting_several, export_name, group_by_project, menu_label, next_filter, passes, pins,
        project_label, span, still, toggled, waiting_tip, write_exports,
    };
    use gpui::{point, px};
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
    fn the_filter_menu_says_what_each_filter_holds_in_words() {
        // A filter that holds nothing is named and no more.
        assert_eq!(menu_label(SessionFilter::Working, 0), "Working");
        assert_eq!(menu_label(SessionFilter::NeedsYou, 2), "Needs you · 2");
        assert_eq!(
            waiting_tip(SessionFilter::NeedsYou, 1),
            "1 session needs you"
        );
        assert_eq!(
            waiting_tip(SessionFilter::Unseen, 3),
            "3 sessions have something you have not seen"
        );
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

    #[test]
    fn a_session_is_held_only_by_what_runs_in_it() {
        // A reply in another session no longer keeps this one.
        assert_eq!(delete_refusal(1, Some(2), false, false), None);
        assert_eq!(delete_refusal(1, None, false, false), None);
        assert!(delete_refusal(1, Some(1), false, false).is_some());
        assert!(delete_refusal(1, None, true, false).is_some());
        assert!(delete_refusal(1, Some(2), false, true).is_some());
    }

    /// A store of sessions with these ids, in this order.
    fn with_ids(ids: &[u64]) -> Store {
        Store {
            conversations: (ids.iter())
                .map(|&id| Conversation {
                    id,
                    ..Default::default()
                })
                .collect(),
            ..Default::default()
        }
    }

    #[test]
    fn the_session_on_screen_stays_when_another_is_deleted() {
        // Session 3 sat at 2; 1 was deleted from 0, so it is at 1 now.
        assert_eq!(active_after(&with_ids(&[2, 3, 4]), 3, 0), 1);
        // One after it was deleted: it has not moved.
        assert_eq!(active_after(&with_ids(&[1, 2, 4]), 2, 2), 1);
        // It was the one deleted: the one listed under it, older, shows.
        assert_eq!(active_after(&with_ids(&[1, 2, 4]), 3, 2), 1);
        assert_eq!(active_after(&with_ids(&[2, 4]), 1, 0), 0);
    }

    #[test]
    fn the_sidebar_keys_go_on_to_the_next_session_listed() {
        let listed = [7, 2, 5];
        assert_eq!(beside(&listed, 7), Some(2));
        assert_eq!(beside(&listed, 2), Some(5));
        // The last one goes back up.
        assert_eq!(beside(&listed, 5), Some(2));
        assert_eq!(beside(&[7], 7), None);
        assert_eq!(beside(&listed, 9), None);
    }

    #[test]
    fn a_click_with_cmd_adds_a_session_or_takes_it_out() {
        assert_eq!(toggled(vec![], 4), [4]);
        assert_eq!(toggled(vec![4, 7], 9), [4, 7, 9]);
        assert_eq!(toggled(vec![4, 7, 9], 7), [4, 9]);
        assert!(toggled(vec![4], 4).is_empty());
    }

    #[test]
    fn a_shift_click_selects_every_row_between_either_way() {
        // Ids as the sidebar lists them, top to bottom.
        let listed = [8, 3, 5, 1, 9];
        assert_eq!(span(&listed, 3, 1), [3, 5, 1]);
        assert_eq!(span(&listed, 1, 3), [3, 5, 1]);
        assert_eq!(span(&listed, 5, 5), [5]);
        // From a session the search has hidden: the one clicked alone.
        assert_eq!(span(&listed, 7, 1), [1]);
    }

    #[test]
    fn one_pin_pins_them_all_unless_all_are_pinned() {
        assert!(pins([false, false]));
        assert!(pins([true, false, true]));
        assert!(!pins([true, true]));
    }

    #[test]
    fn deleting_several_names_them_while_they_are_few() {
        let titles =
            |names: &[&str]| -> Vec<String> { names.iter().map(|name| name.to_string()).collect() };
        assert_eq!(
            deleting_several(&titles(&["Fix the build"]), 0),
            "\"Fix the build\" and its messages will be removed from this device."
        );
        assert_eq!(
            deleting_several(&titles(&["A", "B", "C"]), 0),
            "3 sessions and their messages will be removed from this device: \"A\", \"B\" and \"C\"."
        );
        assert_eq!(
            deleting_several(&titles(&["A", "B", "C", "D", "E"]), 2),
            "5 sessions and their messages will be removed from this device: \"A\", \"B\" and 3 more. \
             2 more have something running in them and stay."
        );
        assert!(
            deleting_several(&titles(&["A", "B"]), 1)
                .ends_with("1 more has something running in it and stays.")
        );
    }

    #[test]
    fn an_export_is_named_for_its_session_and_never_written_over() {
        assert_eq!(export_name("Fix: the build?"), "Fix- the build-");
        let folder =
            std::env::temp_dir().join(format!("letronna-export-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&folder);
        std::fs::create_dir_all(&folder).unwrap();
        let files = [
            ("Same".to_string(), "first".to_string()),
            ("Same".to_string(), "second".to_string()),
            (String::new(), "untitled".to_string()),
        ];
        assert_eq!(write_exports(&folder, &files), 0);
        let read = |name: &str| std::fs::read_to_string(folder.join(name)).unwrap();
        assert_eq!(read("Same.md"), "first");
        assert_eq!(read("Same 2.md"), "second");
        assert_eq!(read("Session.md"), "untitled");
        let _ = std::fs::remove_dir_all(&folder);
    }

    #[test]
    fn a_press_held_still_or_nearly_is_a_long_press() {
        let at = point(px(100.), px(40.));
        assert!(still(at, at));
        assert!(still(at, point(px(105.), px(44.))));
        assert!(!still(at, point(px(100.), px(49.))));
    }
}
