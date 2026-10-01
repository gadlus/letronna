//! The Changes tab of the right panel, and what the status bar says of the
//! project: either what this session's tools changed, measured from the
//! copies rewind keeps, or the working tree as git sees it. A file's diff
//! is read when it is opened, off the UI thread.
//!
//! In the working tree scope the files are grouped as git stages them, and
//! a file or one hunk of it can be staged or unstaged, then the staged set
//! committed, and pushed, from a message box under the list.
//!
//! Nothing here polls. The list and the status are read again after a tool
//! runs, when a turn finishes, when the window comes back to the front or
//! the tab gets the keys, when the session or its folder changes, and after
//! a revert, a stage or a commit: the moments they can have moved.

use crate::prelude::*;
use gpui_component::Selectable;
use gpui_component::button::{ButtonGroup, ButtonVariant};
use gpui_component::dialog::DialogButtonProps;
use gpui_component::notification::Notification;
use letronna_core::commit::{self, Entry, FileDiff, Kind, Tree};
use letronna_core::gate::{Gate, Outcome};
use letronna_core::git::{self, ChangedFile};
use letronna_core::history;
use std::path::Path;

actions!(
    changes,
    [
        ChangesTab,
        NextChange,
        PreviousChange,
        OpenChange,
        SwitchScope,
        AddChange,
        RevertChange,
        LeaveChanges,
        ToggleStage,
        NextHunk,
        PreviousHunk,
        ToggleHunk,
        WriteMessage,
        SuggestMessage,
        CommitStaged,
        CommitAndPush,
        LeaveMessage
    ]
);

/// Patch lines drawn under a file, past which the rest are counted: a
/// generated file's thousands of lines would each be an element.
const SHOWN_LINES: usize = 1500;

/// Characters drawn of one patch line: a minified file is one line of the
/// whole patch, laid out again on every frame the tab is open.
const LINE_CHARS: usize = 1000;

/// A row of the file list; the list is virtual, so every row is this tall.
const ROW: Pixels = px(24.);

/// Rows the list shows before it scrolls, leaving the rest to the diff.
const ROWS_SHOWN: usize = 10;

/// Which changes the tab lists.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub(crate) enum Scope {
    /// What this session's tools wrote, from how the session found it.
    #[default]
    Session,
    /// Everything git sees changed since the last commit.
    Tree,
}

/// The tab's list, the file open in it, and the project's git status.
pub(crate) struct Changes {
    scope: Scope,
    /// The session and folder last read, so a switch to another reads again.
    read_for: Option<(u64, Option<PathBuf>)>,
    /// This session's files, or why there are none; `None` until read.
    files: Option<Result<Vec<ChangedFile>, String>>,
    /// The working tree's staged and unstaged files, or why there are none.
    tree: Option<Result<Tree, String>>,
    selected: usize,
    open: Option<Open>,
    /// The project folder's status, with the folder it is of.
    status: Option<(PathBuf, git::Status)>,
    /// A read is running, and another is wanted once it is back.
    reading: bool,
    again: bool,
    /// A stage, a commit or a push is running; the buttons wait for it.
    working: bool,
    /// The model is writing a commit message.
    suggesting: bool,
    /// The commit message, kept across reads until a commit takes it.
    message: Entity<InputState>,
    pub(crate) focus: FocusHandle,
    scroll: UniformListScrollHandle,
    /// Diffs asked for so far, to tell each answer's ask apart.
    asked: u64,
}

struct Open {
    session: u64,
    scope: Scope,
    file: PathBuf,
    /// In the working tree: the file as git lists it, and whether its staged
    /// side is the one open.
    entry: Option<(Entry, bool)>,
    /// Which ask of git this is, so an answer to an earlier one is dropped.
    ask: u64,
    /// `None` while it is still being read.
    diff: Option<Result<String, String>>,
    /// In the working tree: the open side cut into hunks, and the one the
    /// keys stage or unstage.
    hunks: Option<FileDiff>,
    hunk: usize,
}

impl Open {
    /// Whether this is `file` in `scope` of `session`, on the same side.
    fn is(&self, session: u64, scope: Scope, file: &Path, staged: Option<bool>) -> bool {
        let side = self.entry.as_ref().map(|(_, staged)| *staged);
        self.session == session && self.scope == scope && self.file == file && side == staged
    }
}

/// What one read brings back.
struct Read {
    listing: Listing,
    status: Option<git::Status>,
}

/// The files in a scope, or why there are none.
enum Listing {
    Session(Result<Vec<ChangedFile>, String>),
    Tree(Result<Tree, String>),
}

impl Changes {
    pub(crate) fn new(window: &mut Window, cx: &mut App) -> Self {
        let message = cx.new(|cx| {
            InputState::new(window, cx)
                .multi_line(true)
                .auto_grow(3, 8)
                .placeholder("Commit message")
        });
        Self {
            scope: Scope::default(),
            read_for: None,
            files: None,
            tree: None,
            selected: 0,
            open: None,
            status: None,
            reading: false,
            again: false,
            working: false,
            suggesting: false,
            message,
            focus: cx.focus_handle(),
            scroll: UniformListScrollHandle::new(),
            asked: 0,
        }
    }
}

/// The working tree's files in the order the tab lists them, staged first,
/// each with whether it is the staged side.
fn flat(tree: &Tree) -> Vec<(bool, &Entry)> {
    let staged = tree.staged.iter().map(|entry| (true, entry));
    staged
        .chain(tree.unstaged.iter().map(|entry| (false, entry)))
        .collect()
}

/// The tool results of `messages` with only their file notes: all the
/// session scope reads, without copying the conversation to another thread.
fn file_notes(messages: &[Message]) -> Vec<Message> {
    messages
        .iter()
        .filter(|m| m.role == "tool" && !m.files.is_empty())
        .map(|m| Message {
            role: m.role.clone(),
            files: m.files.clone(),
            ..Default::default()
        })
        .collect()
}

impl Chat {
    /// Whether the tab has anything to list: files this session wrote, or
    /// changes git sees in its folder.
    pub(crate) fn has_changes(&self) -> bool {
        let conversation = &self.store.conversations[self.store.active];
        let written = conversation
            .messages
            .iter()
            .any(|m| m.role == "tool" && !m.files.is_empty());
        written || self.git_status().is_some_and(|status| status.changed > 0)
    }

    /// The git status of the session's folder, once read.
    pub(crate) fn git_status(&self) -> Option<&git::Status> {
        let folder = self.store.conversations[self.store.active]
            .workspace
            .as_ref()?;
        let (of, status) = self.changes.status.as_ref()?;
        (of == folder).then_some(status)
    }

    /// Read the list and the status again when the session on screen or
    /// its folder is not the one they were read for.
    pub(crate) fn keep_changes_current(&mut self, cx: &mut Context<Self>) {
        let conversation = &self.store.conversations[self.store.active];
        let now = Some((conversation.id, conversation.workspace.clone()));
        if self.changes.read_for != now {
            self.changes.read_for = now;
            self.changes.files = None;
            self.changes.tree = None;
            self.changes.selected = 0;
            self.refresh_changes(cx);
        }
    }

    /// Read the list and the status again off the UI thread, as something
    /// may have changed them. A read asked for while one runs waits for it.
    pub(crate) fn refresh_changes(&mut self, cx: &mut Context<Self>) {
        if self.changes.reading {
            self.changes.again = true;
            return;
        }
        let conversation = &self.store.conversations[self.store.active];
        let (session, scope) = (conversation.id, self.changes.scope);
        let folder = conversation.workspace.clone();
        let notes = match scope {
            Scope::Session => file_notes(&conversation.messages),
            Scope::Tree => Vec::new(),
        };
        self.changes.reading = true;
        let read = cx.background_executor().spawn({
            let folder = folder.clone();
            async move {
                let Some(folder) = folder else {
                    let none = "This session has no project folder.".to_owned();
                    let listing = match scope {
                        Scope::Session => Listing::Session(Err(none)),
                        Scope::Tree => Listing::Tree(Err(none)),
                    };
                    return Read {
                        listing,
                        status: None,
                    };
                };
                let listing = match scope {
                    Scope::Session => {
                        Listing::Session(Ok(history::session_files(session, &notes, &folder)))
                    }
                    Scope::Tree => Listing::Tree(commit::tree(&folder)),
                };
                Read {
                    listing,
                    status: git::status(&folder).ok(),
                }
            }
        });
        cx.spawn(async move |this, cx| {
            let read = read.await;
            let _ = this.update(cx, |this, cx| {
                this.changes.reading = false;
                let still = this.store.conversations[this.store.active].id == session
                    && this.changes.scope == scope;
                if still {
                    match read.listing {
                        Listing::Session(files) => this.took_changes(files, cx),
                        Listing::Tree(tree) => this.took_tree(tree, cx),
                    }
                }
                if let Some(folder) = folder {
                    this.changes.status = read.status.map(|status| (folder, status));
                }
                if std::mem::take(&mut this.changes.again) || !still {
                    this.refresh_changes(cx);
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Keep a list just read, with the selection on the same file where it
    /// is still there, and the open diff read again, as it may have moved.
    fn took_changes(&mut self, files: Result<Vec<ChangedFile>, String>, cx: &mut Context<Self>) {
        let was = self.selected_change().map(|file| file.path.clone());
        if let (Some(was), Ok(list)) = (was, &files) {
            self.changes.selected = list.iter().position(|f| f.path == was).unwrap_or(0);
        }
        self.changes.files = Some(files);
        if self.changes.open.is_some() {
            self.read_open_diff(cx);
        }
    }

    /// Keep the working tree just read. The selection stays on the same
    /// file and side where it still is, or else at the same place, so
    /// staging files one after another walks down the list; the open diff
    /// follows the selection when its side of the file has gone.
    fn took_tree(&mut self, tree: Result<Tree, String>, cx: &mut Context<Self>) {
        let was = self
            .selected_entry()
            .map(|(staged, entry)| (staged, entry.path.clone()));
        if let Ok(list) = &tree {
            let list = flat(list);
            let same = was.and_then(|(staged, path)| {
                list.iter()
                    .position(|(side, entry)| *side == staged && entry.path == path)
            });
            let at = same.unwrap_or(self.changes.selected);
            self.changes.selected = at.min(list.len().saturating_sub(1));
        }
        self.changes.tree = Some(tree);
        let Some(open) = self
            .changes
            .open
            .as_ref()
            .filter(|o| o.scope == Scope::Tree)
        else {
            return;
        };
        let still = open.entry.as_ref().is_some_and(|(open, staged)| {
            self.tree_entries()
                .iter()
                .any(|(side, entry)| side == staged && *entry == open)
        });
        if still {
            self.read_open_diff(cx);
        } else {
            self.changes.open = None;
            self.open_selected(false, cx);
        }
    }

    fn selected_change(&self) -> Option<&ChangedFile> {
        match &self.changes.files {
            Some(Ok(files)) if self.changes.scope == Scope::Session => {
                files.get(self.changes.selected)
            }
            _ => None,
        }
    }

    /// The working tree's files as listed, or none before it is read.
    fn tree_entries(&self) -> Vec<(bool, &Entry)> {
        match &self.changes.tree {
            Some(Ok(tree)) => flat(tree),
            _ => Vec::new(),
        }
    }

    /// The selected file in the working tree scope, and whether it is the
    /// staged side of it.
    fn selected_entry(&self) -> Option<(bool, &Entry)> {
        if self.changes.scope != Scope::Tree {
            return None;
        }
        self.tree_entries().get(self.changes.selected).copied()
    }

    /// The selected file's path in either scope.
    fn selected_path(&self) -> Option<PathBuf> {
        match self.changes.scope {
            Scope::Session => self.selected_change().map(|file| file.path.clone()),
            Scope::Tree => self.selected_entry().map(|(_, entry)| entry.path.clone()),
        }
    }

    /// How many files the tab lists in its scope.
    fn listed(&self) -> usize {
        match (self.changes.scope, &self.changes.files) {
            (Scope::Session, Some(Ok(files))) => files.len(),
            (Scope::Session, _) => 0,
            (Scope::Tree, _) => self.tree_entries().len(),
        }
    }

    /// Show the Changes tab with the keys in its list.
    pub(crate) fn show_changes(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.show_tab(crate::ui::panel::PanelTab::Changes, window, cx);
        self.refresh_changes(cx);
        window.focus(&self.changes.focus);
    }

    fn switch_scope(&mut self, scope: Scope, cx: &mut Context<Self>) {
        if self.changes.scope == scope {
            return;
        }
        self.changes.scope = scope;
        self.changes.files = None;
        self.changes.tree = None;
        self.changes.selected = 0;
        self.changes.open = None;
        self.refresh_changes(cx);
        cx.notify();
    }

    /// Move the selection, and the open diff with it, so what Revert and
    /// Add to message act on is always the file shown.
    fn move_selection(&mut self, by: isize, cx: &mut Context<Self>) {
        let count = self.listed();
        if count == 0 {
            return;
        }
        let at = self
            .changes
            .selected
            .saturating_add_signed(by)
            .min(count - 1);
        self.changes.selected = at;
        // The working tree's list has a heading row above each group.
        let row = match self.selected_entry() {
            Some((true, _)) => at + 1,
            Some((false, _)) => at + 2,
            None => at,
        };
        self.changes
            .scroll
            .scroll_to_item(row, ScrollStrategy::Center);
        if self.changes.open.is_some() {
            self.open_selected(false, cx);
        }
        cx.notify();
    }

    /// Show the selected file's diff; `toggle` hides it instead when it is
    /// the one open.
    fn open_selected(&mut self, toggle: bool, cx: &mut Context<Self>) {
        let Some(file) = self.selected_path() else {
            return;
        };
        let entry = self
            .selected_entry()
            .map(|(staged, entry)| (entry.clone(), staged));
        let side = entry.as_ref().map(|(_, staged)| *staged);
        let session = self.store.conversations[self.store.active].id;
        let scope = self.changes.scope;
        let open_now = self
            .changes
            .open
            .as_ref()
            .is_some_and(|open| open.is(session, scope, &file, side));
        if open_now {
            if toggle {
                self.changes.open = None;
                cx.notify();
            }
            return;
        }
        self.changes.open = Some(Open {
            session,
            scope,
            file,
            entry,
            ask: 0,
            diff: None,
            hunks: None,
            hunk: 0,
        });
        self.read_open_diff(cx);
    }

    /// Read the open file's diff off the UI thread. What was shown of it
    /// stays until the new one is back, so a reread does not flicker.
    fn read_open_diff(&mut self, cx: &mut Context<Self>) {
        let conversation = &self.store.conversations[self.store.active];
        let Some(open) = self
            .changes
            .open
            .as_mut()
            .filter(|open| open.session == conversation.id)
        else {
            return;
        };
        let Some(folder) = conversation.workspace.clone() else {
            return;
        };
        // Each read is numbered, so a slower earlier one never lands over
        // the answer to a later one.
        self.changes.asked += 1;
        let ask = self.changes.asked;
        open.ask = ask;
        let (session, scope, file) = (open.session, open.scope, open.file.clone());
        let entry = open.entry.clone();
        let side = entry.as_ref().map(|(_, staged)| *staged);
        let notes = file_notes(&conversation.messages);
        let asked = file.clone();
        let diff = cx.background_executor().spawn(async move {
            match (scope, entry) {
                (Scope::Tree, Some((entry, staged))) => commit::file_diff(&folder, &entry, staged)
                    .map(|diff| {
                        let text = diff.text();
                        let said = if text.trim().is_empty() {
                            "No change on this side.".to_owned()
                        } else {
                            text
                        };
                        (said, Some(diff))
                    }),
                _ => history::session_patch(session, &notes, &folder, &asked)
                    .map(|text| (text, None)),
            }
        });
        cx.spawn(async move |this, cx| {
            let diff = diff.await;
            let _ = this.update(cx, |this, cx| {
                // Only for this ask of the file and side still open: a later
                // click, or a later read of the same file, in this session
                // or another with its own project, wins.
                if let Some(open) = this
                    .changes
                    .open
                    .as_mut()
                    .filter(|open| open.ask == ask && open.is(session, scope, &file, side))
                {
                    let (text, hunks) = match diff {
                        Ok((text, hunks)) => (Ok(text), hunks),
                        Err(why) => (Err(why), None),
                    };
                    let count = hunks.as_ref().map_or(0, |diff| diff.hunks.len());
                    open.hunk = open.hunk.min(count.saturating_sub(1));
                    open.diff = Some(text);
                    open.hunks = hunks;
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    /// Put the selected file's full path in the composer, as the attach
    /// menu does, so the next message can ask about it.
    fn add_change_to_message(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(folder) = self.store.conversations[self.store.active]
            .workspace
            .clone()
        else {
            return;
        };
        let Some(file) = self.selected_path() else {
            return;
        };
        let text = format!("[attached: {}]\n", folder.join(file).display());
        self.insert(&text, window, cx);
        self.focus_composer(window, cx);
        // Setting a many-line box's text puts its cursor at the start, and
        // what is typed next goes on after the attached path.
        self.composer.update(cx, |input, cx| {
            use gpui_component::input::RopeExt;
            let end = input.text().offset_to_position(input.value().len());
            input.set_cursor_position(end, window, cx);
        });
    }

    /// Run `act` on the project folder off the UI thread, then say what
    /// went wrong, if anything, and read the tab again. One at a time: the
    /// buttons wait while one runs.
    fn git_action(
        &mut self,
        act: impl FnOnce(&Path) -> Result<(), String> + Send + 'static,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.changes.working {
            return;
        }
        let Some(folder) = self.store.conversations[self.store.active]
            .workspace
            .clone()
        else {
            return;
        };
        self.changes.working = true;
        let done = cx.background_executor().spawn(async move { act(&folder) });
        cx.spawn_in(window, async move |this, cx| {
            let done = done.await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.changes.working = false;
                if let Err(why) = done {
                    window.push_notification(Notification::warning(why), cx);
                }
                this.refresh_changes(cx);
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    /// Stage the selected file whole, or unstage it when it is the staged
    /// side that is selected.
    fn toggle_stage(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some((staged, entry)) = self.selected_entry().map(|(s, e)| (s, e.clone())) else {
            return;
        };
        self.git_action(
            move |folder| match staged {
                true => commit::unstage(folder, &entry),
                false => commit::stage(folder, &entry),
            },
            window,
            cx,
        );
    }

    /// Move the hunk the keys act on within the open diff.
    fn move_hunk(&mut self, by: isize, cx: &mut Context<Self>) {
        let Some(open) = self.changes.open.as_mut() else {
            return;
        };
        let count = open.hunks.as_ref().map_or(0, |diff| diff.hunks.len());
        if count > 0 {
            open.hunk = open.hunk.saturating_add_signed(by).min(count - 1);
            cx.notify();
        }
    }

    /// Stage hunk `at` of the open diff, or unstage it on the staged side.
    fn toggle_hunk(&mut self, at: usize, window: &mut Window, cx: &mut Context<Self>) {
        let session = self.store.conversations[self.store.active].id;
        let Some(diff) = self
            .changes
            .open
            .as_ref()
            .filter(|open| open.session == session && open.scope == Scope::Tree)
            .and_then(|open| open.hunks.clone())
        else {
            return;
        };
        if let Some(open) = self.changes.open.as_mut() {
            open.hunk = at;
        }
        self.git_action(
            move |folder| match diff.staged {
                true => commit::unstage_hunk(folder, &diff, at),
                false => commit::stage_hunk(folder, &diff, at),
            },
            window,
            cx,
        );
    }

    /// Put the message box's cursor in, for typing.
    fn write_message(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.changes.scope == Scope::Tree {
            self.changes
                .message
                .update(cx, |input, cx| input.focus(window, cx));
        }
    }

    /// Have the session's model write a message for what is staged, in
    /// place of what the box holds, which the user can then edit.
    fn suggest_message(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.changes.suggesting || self.changes.scope != Scope::Tree {
            return;
        }
        let Some(folder) = self.store.conversations[self.store.active]
            .workspace
            .clone()
        else {
            return;
        };
        let connection = match self.connection_for(self.store.active, None) {
            Ok(connection) => connection,
            Err(why) => {
                window.push_notification(Notification::warning(why), cx);
                return;
            }
        };
        self.changes.suggesting = true;
        let written = cx
            .background_executor()
            .spawn(async move { commit::suggest(&connection, &folder) });
        cx.spawn_in(window, async move |this, cx| {
            let written = written.await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.changes.suggesting = false;
                match written {
                    Ok(message) => this.changes.message.update(cx, |input, cx| {
                        input.set_value(message, window, cx);
                        input.focus(window, cx);
                    }),
                    Err(why) => window.push_notification(Notification::warning(why), cx),
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    /// Commit what is staged with the message in the box, then push the
    /// branch when `and_push`.
    fn commit_staged(&mut self, and_push: bool, window: &mut Window, cx: &mut Context<Self>) {
        // A reply may be writing files the commit would take half of.
        if self.changes.working || self.busy || self.changes.scope != Scope::Tree {
            return;
        }
        let Some(folder) = self.store.conversations[self.store.active]
            .workspace
            .clone()
        else {
            return;
        };
        let message = self.changes.message.read(cx).value().to_string();
        self.changes.working = true;
        let made = cx
            .background_executor()
            .spawn(async move { commit::commit(&folder, &message) });
        cx.spawn_in(window, async move |this, cx| {
            let made = made.await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.changes.working = false;
                match made {
                    Ok(said) => {
                        this.changes
                            .message
                            .update(cx, |input, cx| input.set_value("", window, cx));
                        window.push_notification(Notification::success(said), cx);
                        if and_push {
                            this.push_branch(window, cx);
                        }
                    }
                    Err(why) => window.push_notification(Notification::warning(why), cx),
                }
                this.refresh_changes(cx);
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    /// Push the branch to its upstream as the session's gate decides a
    /// `git push` command: at once, after asking, or not at all.
    fn push_branch(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let conversation = &self.store.conversations[self.store.active];
        let Some(folder) = conversation.workspace.clone() else {
            return;
        };
        let session = conversation.id;
        let planned = cx.background_executor().spawn({
            let folder = folder.clone();
            async move { commit::plan_push(&folder) }
        });
        cx.spawn_in(window, async move |this, cx| {
            let planned = planned.await;
            let _ = this.update_in(cx, |this, window, cx| {
                let plan = match planned {
                    Ok(plan) => plan,
                    Err(why) => {
                        window.push_notification(Notification::warning(why), cx);
                        return;
                    }
                };
                let mut ctx = letronna_core::tools::ToolContext::default();
                ctx.conversation = session;
                ctx.workspace = Some(folder.clone());
                let gate = Gate::for_window(
                    &ctx,
                    &this.session_allow,
                    &this.rules,
                    this.mode(session),
                    this.unattended(session),
                );
                match plan.decide(&gate) {
                    Outcome::Run => this.run_push(folder, plan, window, cx),
                    Outcome::Refuse(why) => {
                        window.push_notification(Notification::warning(why), cx)
                    }
                    Outcome::Ask => this.ask_push(session, folder, plan, window, cx),
                }
            });
        })
        .detach();
    }

    /// Ask whether to push, as a command's approval card would.
    fn ask_push(
        &mut self,
        session: u64,
        folder: PathBuf,
        plan: commit::Push,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let title = format!("Push {} to {}", plan.branch, plan.remote);
        let command = plan.command();
        let chat = cx.entity();
        window.open_dialog(cx, move |dialog, _, cx| {
            let (chat, folder, plan) = (chat.clone(), folder.clone(), plan.clone());
            dialog
                .title(div().font_family(HEADING_FONT).child(title.clone()))
                .w(px(460.))
                .confirm()
                .button_props(DialogButtonProps::default().ok_text("Push"))
                .child(
                    v_flex()
                        .gap_2()
                        .text_sm()
                        .text_color(cx.theme().muted_foreground)
                        .child("This session's permission mode asks before a command runs.")
                        .child(
                            div()
                                .font_family(MONO_FONT)
                                .text_color(cx.theme().foreground)
                                .child(command.clone()),
                        ),
                )
                .on_ok(move |_, window, cx| {
                    chat.update(cx, |this, cx| {
                        if this.store.conversations[this.store.active].id == session {
                            this.run_push(folder.clone(), plan.clone(), window, cx);
                        }
                    });
                    true
                })
        });
    }

    /// Push, off the UI thread, and say how it went.
    fn run_push(
        &mut self,
        folder: PathBuf,
        plan: commit::Push,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.changes.working = true;
        let pushed = cx
            .background_executor()
            .spawn(async move { commit::push(&folder, &plan) });
        cx.spawn_in(window, async move |this, cx| {
            let pushed = pushed.await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.changes.working = false;
                let note = match pushed {
                    Ok(said) => Notification::success(said),
                    Err(why) => Notification::warning(why),
                };
                window.push_notification(note, cx);
                // The status bar's ahead count moved.
                this.refresh_changes(cx);
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    /// Ask whether to undo this session's writes to the selected file, then
    /// do: only in the session scope, as those are the writes rewind kept.
    fn revert_change(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.changes.scope != Scope::Session || self.busy {
            return;
        }
        let conversation = &self.store.conversations[self.store.active];
        let Some(folder) = conversation.workspace.clone() else {
            return;
        };
        let Some(file) = self.selected_change().map(|file| file.path.clone()) else {
            return;
        };
        let session = conversation.id;
        let notes = file_notes(&conversation.messages);
        // Each write is hashed again, which a big file makes slow.
        let planned = cx.background_executor().spawn({
            let (folder, file) = (folder.clone(), file.clone());
            async move {
                let plan = history::revert_plan(session, &notes, &folder, &file);
                let rows = history::listing(&plan, Some(&folder));
                (plan, rows)
            }
        });
        cx.spawn_in(window, async move |this, cx| {
            let (plan, rows) = planned.await;
            let _ = this.update_in(cx, |this, window, cx| {
                if this.busy || this.store.conversations[this.store.active].id != session {
                    return;
                }
                if !plan.iter().any(|restore| restore.fate.touches()) {
                    let said = rows.first().cloned().unwrap_or_else(|| {
                        format!("{} is as the session found it.", file.display())
                    });
                    window.push_notification(Notification::info(said), cx);
                    return;
                }
                this.ask_revert(session, file, plan, rows, window, cx);
            });
        })
        .detach();
    }

    fn ask_revert(
        &mut self,
        session: u64,
        file: PathBuf,
        plan: Vec<history::Restore>,
        rows: Vec<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let title = format!("Revert {}", file.display());
        let body = "This session's edits to it are undone, back to how the session found it. \
                    What it is now is kept first, so this can be undone by hand.";
        let chat = cx.entity();
        window.open_dialog(cx, move |dialog, _, cx| {
            let (chat, plan) = (chat.clone(), plan.clone());
            dialog
                .title(div().font_family(HEADING_FONT).child(title.clone()))
                .w(px(460.))
                .confirm()
                .button_props(
                    DialogButtonProps::default()
                        .ok_text("Revert")
                        .ok_variant(ButtonVariant::Danger),
                )
                .child(
                    v_flex()
                        .gap_2()
                        .text_sm()
                        .text_color(cx.theme().muted_foreground)
                        .child(body)
                        .children(rows.iter().map(|row| div().child(row.clone()))),
                )
                .on_ok(move |_, window, cx| {
                    chat.update(cx, |this, cx| {
                        // A reply may have started, or the session changed,
                        // while the question was up.
                        if this.busy || this.store.conversations[this.store.active].id != session {
                            return;
                        }
                        this.put_files_back(session, plan.clone(), window, cx);
                    });
                    true
                })
        });
    }

    /// The tab's body: the scope switch, the files in scope with their
    /// counts, and under them the open file's diff.
    pub(crate) fn changes_tab(&self, cx: &mut Context<Self>) -> AnyElement {
        let conversation = &self.store.conversations[self.store.active];
        let scope = self.changes.scope;
        let switch = ButtonGroup::new("changes-scope")
            .xsmall()
            .child(
                Button::new("scope-session")
                    .framed(cx)
                    .label("This session")
                    .selected(scope == Scope::Session),
            )
            .child(
                Button::new("scope-tree")
                    .framed(cx)
                    .label("Working tree")
                    .selected(scope == Scope::Tree),
            )
            .on_click(cx.listener(|this, clicked: &Vec<usize>, _, cx| {
                let scope = match clicked.first() {
                    Some(1) => Scope::Tree,
                    _ => Scope::Session,
                };
                this.switch_scope(scope, cx);
            }));
        let header = h_flex()
            .px_2()
            .py_1p5()
            .gap_2()
            .items_center()
            .child(switch)
            .child(div().flex_1())
            .child(
                Button::new("changes-folder")
                    .plain(cx)
                    .xsmall()
                    .icon(Icon::new(AppIcon::Folder).size_3())
                    .tooltip("Change the project folder")
                    .on_click(cx.listener(|this, _, window, cx| this.attach_workspace(window, cx))),
            );
        let open = self
            .changes
            .open
            .as_ref()
            .filter(|open| open.session == conversation.id && open.scope == scope);
        let listed = |list: AnyElement, cx: &mut Context<Self>| {
            v_flex()
                .flex_1()
                .min_h_0()
                .child(list)
                .children(open.map(|open| self.open_change(open, cx)))
                .into_any_element()
        };
        let mut committing = false;
        let body = match scope {
            Scope::Session => match &self.changes.files {
                None => note("Reading…", cx),
                Some(Err(why)) => note(why, cx),
                Some(Ok(files)) if files.is_empty() => {
                    note("This session has changed no files in its project.", cx)
                }
                Some(Ok(files)) => {
                    let list = self.file_list(files.len(), cx).into_any_element();
                    listed(list, cx)
                }
            },
            Scope::Tree => match &self.changes.tree {
                None => note("Reading…", cx),
                Some(Err(why)) => note(why, cx),
                Some(Ok(tree)) if tree.staged.is_empty() && tree.unstaged.is_empty() => {
                    note("Nothing has changed since the last commit.", cx)
                }
                Some(Ok(tree)) => {
                    committing = true;
                    let list = self.tree_list(tree, cx).into_any_element();
                    listed(list, cx)
                }
            },
        };
        // The message box is outside the list's key context, so the list's
        // single-letter keys do not take the letters typed into it.
        let list = v_flex()
            .id("changes")
            .flex_1()
            .min_h_0()
            .key_context("Changes")
            .track_focus(&self.changes.focus)
            .on_action(cx.listener(|this, _: &NextChange, _, cx| this.move_selection(1, cx)))
            .on_action(cx.listener(|this, _: &PreviousChange, _, cx| this.move_selection(-1, cx)))
            .on_action(cx.listener(|this, _: &OpenChange, _, cx| this.open_selected(true, cx)))
            .on_action(cx.listener(|this, _: &SwitchScope, _, cx| {
                let other = match this.changes.scope {
                    Scope::Session => Scope::Tree,
                    Scope::Tree => Scope::Session,
                };
                this.switch_scope(other, cx);
            }))
            .on_action(
                cx.listener(|this, _: &AddChange, window, cx| {
                    this.add_change_to_message(window, cx)
                }),
            )
            .on_action(
                cx.listener(|this, _: &RevertChange, window, cx| this.revert_change(window, cx)),
            )
            .on_action(
                cx.listener(|this, _: &LeaveChanges, window, cx| this.focus_composer(window, cx)),
            )
            .on_action(
                cx.listener(|this, _: &ToggleStage, window, cx| this.toggle_stage(window, cx)),
            )
            .on_action(cx.listener(|this, _: &NextHunk, _, cx| this.move_hunk(1, cx)))
            .on_action(cx.listener(|this, _: &PreviousHunk, _, cx| this.move_hunk(-1, cx)))
            .on_action(cx.listener(|this, _: &ToggleHunk, window, cx| {
                let at = this.changes.open.as_ref().map_or(0, |open| open.hunk);
                this.toggle_hunk(at, window, cx);
            }))
            .on_action(
                cx.listener(|this, _: &WriteMessage, window, cx| this.write_message(window, cx)),
            )
            .on_action(
                cx.listener(|this, _: &SuggestMessage, window, cx| {
                    this.suggest_message(window, cx)
                }),
            )
            .child(header)
            .child(body);
        v_flex()
            .size_full()
            .min_h_0()
            .child(list)
            .when(committing, |tab| tab.child(self.commit_box(cx)))
            .into_any_element()
    }

    /// The message box under the working tree's list, with Suggest, Commit
    /// and Commit and push.
    fn commit_box(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let staged = matches!(&self.changes.tree, Some(Ok(tree)) if !tree.staged.is_empty());
        // An empty message is said when Commit is pressed: the box does not
        // redraw the tab as it is typed in.
        let can_commit = staged && !self.changes.working && !self.busy;
        let focus = self.changes.focus.clone();
        v_flex()
            .p_2()
            .gap_1p5()
            .flex_shrink_0()
            .border_t_1()
            .border_color(cx.theme().border)
            .key_context("CommitMessage")
            .on_action(cx.listener(|this, _: &CommitStaged, window, cx| {
                this.commit_staged(false, window, cx)
            }))
            .on_action(cx.listener(|this, _: &CommitAndPush, window, cx| {
                this.commit_staged(true, window, cx)
            }))
            .on_action(cx.listener(move |_, _: &LeaveMessage, window, _| window.focus(&focus)))
            .child(Input::new(&self.changes.message).small().w_full())
            .child(
                h_flex()
                    .gap_1()
                    .items_center()
                    .child(
                        Button::new("suggest-message")
                            .framed(cx)
                            .xsmall()
                            .label("Suggest")
                            .loading(self.changes.suggesting)
                            .disabled(!staged || self.changes.suggesting)
                            .tooltip_with_action(
                                "Have the model write a message for what is staged",
                                &SuggestMessage,
                                Some("Changes"),
                            )
                            .on_click(
                                cx.listener(|this, _, window, cx| this.suggest_message(window, cx)),
                            ),
                    )
                    .child(div().flex_1())
                    .child(
                        Button::new("commit-staged")
                            .framed(cx)
                            .xsmall()
                            .label("Commit")
                            .disabled(!can_commit)
                            .tooltip_with_action(
                                "Commit what is staged",
                                &CommitStaged,
                                Some("CommitMessage > Input"),
                            )
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.commit_staged(false, window, cx)
                            })),
                    )
                    .child(
                        Button::new("commit-push")
                            .framed(cx)
                            .xsmall()
                            .label("Commit and push")
                            .loading(self.changes.working)
                            .disabled(!can_commit)
                            .tooltip_with_action(
                                "Commit, then push the branch to its upstream",
                                &CommitAndPush,
                                Some("CommitMessage > Input"),
                            )
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.commit_staged(true, window, cx)
                            })),
                    ),
            )
    }

    /// The working tree's files as a virtual list, a heading above the
    /// staged group and above the unstaged one.
    fn tree_list(&self, tree: &Tree, cx: &mut Context<Self>) -> impl IntoElement {
        let count = tree.staged.len() + tree.unstaged.len() + 2;
        let height = ROW * count.min(ROWS_SHOWN) as f32;
        uniform_list(
            "tree-files",
            count,
            cx.processor(|this, range: std::ops::Range<usize>, _, cx| {
                let Some(Ok(tree)) = &this.changes.tree else {
                    return Vec::new();
                };
                let staged = tree.staged.len();
                let focused = this.changes.focus.clone();
                range
                    .filter_map(|row| {
                        // Rows 0 and staged + 1 are the headings.
                        let (group, at) = match row {
                            0 => return Some(group_row("Staged", staged, cx)),
                            row if row == staged + 1 => {
                                return Some(group_row("Unstaged", tree.unstaged.len(), cx));
                            }
                            row if row <= staged => (&tree.staged, row - 1),
                            row => (&tree.unstaged, row - staged - 2),
                        };
                        let entry = group.get(at)?;
                        let is_staged = row <= staged;
                        let index = if is_staged { at } else { staged + at };
                        let selected = index == this.changes.selected;
                        let focused = focused.clone();
                        let toggle = Button::new(("stage-file", row))
                            .plain(cx)
                            .xsmall()
                            .label(if is_staged { "Unstage" } else { "Stage" })
                            .disabled(this.changes.working)
                            .on_click(cx.listener(move |this, _, window, cx| {
                                cx.stop_propagation();
                                this.changes.selected = index;
                                this.toggle_stage(window, cx);
                            }));
                        Some(
                            entry_row(entry, selected, cx)
                                .child(toggle)
                                .id(("tree-file", row))
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    this.changes.selected = index;
                                    window.focus(&focused);
                                    this.open_selected(true, cx);
                                }))
                                .into_any_element(),
                        )
                    })
                    .collect()
            }),
        )
        .track_scroll(self.changes.scroll.clone())
        .h(height)
        .flex_shrink_0()
        .border_b_1()
        .border_color(cx.theme().border)
    }

    /// The files in scope as a virtual list: only the rows in view are
    /// built, however many files a generated tree adds.
    fn file_list(&self, count: usize, cx: &mut Context<Self>) -> impl IntoElement {
        let height = ROW * count.min(ROWS_SHOWN) as f32;
        uniform_list(
            "changed-files",
            count,
            cx.processor(|this, range: std::ops::Range<usize>, _, cx| {
                let Some(Ok(files)) = &this.changes.files else {
                    return Vec::new();
                };
                let focused = this.changes.focus.clone();
                range
                    .filter_map(|at| files.get(at).map(|file| (at, file)))
                    .map(|(at, file)| {
                        let focused = focused.clone();
                        file_row(file, at == this.changes.selected, cx)
                            .id(("changed-file", at))
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.changes.selected = at;
                                window.focus(&focused);
                                this.open_selected(true, cx);
                            }))
                            .into_any_element()
                    })
                    .collect()
            }),
        )
        .track_scroll(self.changes.scroll.clone())
        .h(height)
        .flex_shrink_0()
        .border_b_1()
        .border_color(cx.theme().border)
    }

    /// The open file: its name, what can be done with it, and its diff.
    fn open_change(&self, open: &Open, cx: &mut Context<Self>) -> AnyElement {
        let session_scope = open.scope == Scope::Session;
        let side = open.entry.as_ref().map(|(_, staged)| *staged);
        let bar = h_flex()
            .px_2()
            .py_1()
            .gap_1()
            .items_center()
            .border_b_1()
            .border_color(cx.theme().border)
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_xs()
                    .font_family(MONO_FONT)
                    .child(open.file.display().to_string()),
            )
            .when(session_scope, |bar| {
                bar.child(
                    Button::new("revert-change")
                        .plain(cx)
                        .xsmall()
                        .label("Revert")
                        .disabled(self.busy)
                        .tooltip_with_action(
                            "Undo this session's edits to it",
                            &RevertChange,
                            Some("Changes"),
                        )
                        .on_click(
                            cx.listener(|this, _, window, cx| this.revert_change(window, cx)),
                        ),
                )
            })
            .when_some(side, |bar, staged| {
                bar.child(
                    Button::new("stage-open")
                        .plain(cx)
                        .xsmall()
                        .label(if staged { "Unstage file" } else { "Stage file" })
                        .disabled(self.changes.working)
                        .tooltip_with_action(
                            if staged {
                                "Take the whole file out of the next commit"
                            } else {
                                "Put the whole file in the next commit"
                            },
                            &ToggleStage,
                            Some("Changes"),
                        )
                        .on_click(cx.listener(|this, _, window, cx| {
                            let Some((entry, staged)) = this
                                .changes
                                .open
                                .as_ref()
                                .and_then(|open| open.entry.clone())
                            else {
                                return;
                            };
                            this.git_action(
                                move |folder| match staged {
                                    true => commit::unstage(folder, &entry),
                                    false => commit::stage(folder, &entry),
                                },
                                window,
                                cx,
                            );
                        })),
                )
            })
            .child(
                Button::new("add-change")
                    .plain(cx)
                    .xsmall()
                    .label("Add to message")
                    .tooltip_with_action("Name it in the message", &AddChange, Some("Changes"))
                    .on_click(
                        cx.listener(|this, _, window, cx| this.add_change_to_message(window, cx)),
                    ),
            );
        v_flex()
            .flex_1()
            .min_h_0()
            .child(bar)
            .child(
                div()
                    .id("change-diff")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .child(match &open.hunks {
                        // A new file is one hunk, staged with the file.
                        Some(diff) if !diff.untracked && !diff.hunks.is_empty() => {
                            self.hunks(diff, open.hunk, cx)
                        }
                        _ => patch(open, cx),
                    }),
            )
            .into_any_element()
    }
}

impl Chat {
    /// A diff cut into hunks, each under a bar that stages or unstages it
    /// alone; the one the keys act on is marked.
    fn hunks(&self, diff: &FileDiff, chosen: usize, cx: &mut Context<Self>) -> AnyElement {
        let mut shown = 0;
        let mut blocks = Vec::new();
        for (at, hunk) in diff.hunks.iter().enumerate() {
            if shown >= SHOWN_LINES {
                let left: usize = diff.hunks[at..].iter().map(|h| h.lines().count()).sum();
                blocks.push(
                    div()
                        .px_2()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(format!("… {left} more lines"))
                        .into_any_element(),
                );
                break;
            }
            let (head, body) = hunk.split_once('\n').unwrap_or((hunk, ""));
            let lines: Vec<&str> = body.lines().take(SHOWN_LINES - shown).collect();
            shown += lines.len();
            let label = if diff.staged {
                "Unstage hunk"
            } else {
                "Stage hunk"
            };
            let bar =
                h_flex()
                    .px_2()
                    .gap_2()
                    .items_center()
                    .when(at == chosen, |bar| bar.bg(cx.theme().list_active))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_xs()
                            .font_family(MONO_FONT)
                            .text_color(cx.theme().link)
                            .child(head.to_owned()),
                    )
                    .child(
                        Button::new(("toggle-hunk", at))
                            .plain(cx)
                            .xsmall()
                            .label(label)
                            .disabled(self.changes.working)
                            .tooltip_with_action(label, &ToggleHunk, Some("Changes"))
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.toggle_hunk(at, window, cx)
                            })),
                    );
            blocks.push(
                v_flex()
                    .child(bar)
                    .child(patch_lines(lines.into_iter(), cx))
                    .into_any_element(),
            );
        }
        v_flex()
            .py_1()
            .gap_1()
            .child(patch_lines(diff.header.lines(), cx))
            .children(blocks)
            .into_any_element()
    }
}

/// A heading row of the working tree's list: the group and its count.
fn group_row(name: &str, count: usize, cx: &App) -> AnyElement {
    h_flex()
        .h(ROW)
        .px_2()
        .gap_2()
        .items_center()
        .text_xs()
        .text_color(cx.theme().muted_foreground)
        .child(format!("{name} · {count}"))
        .into_any_element()
}

/// A row of the working tree's list: what happened to the file, and its
/// path, with the name it had for a rename.
fn entry_row(entry: &Entry, selected: bool, cx: &App) -> Div {
    let colour = match entry.kind {
        Kind::Added | Kind::Untracked => cx.theme().green,
        Kind::Deleted => cx.theme().danger,
        Kind::Conflicted => cx.theme().warning,
        _ => cx.theme().muted_foreground,
    };
    let path = match &entry.from {
        Some(from) => format!("{} → {}", from.display(), entry.path.display()),
        None => entry.path.display().to_string(),
    };
    h_flex()
        .w_full()
        .h(ROW)
        .pl_2()
        .gap_2()
        .items_center()
        .text_xs()
        .cursor_pointer()
        .when(selected, |row| row.bg(cx.theme().list_active))
        .child(
            div()
                .w(px(10.))
                .font_family(MONO_FONT)
                .text_color(colour)
                .child(entry.kind.letter()),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .truncate()
                .font_family(MONO_FONT)
                .child(path),
        )
}

/// A row of the file list: its path, then what it gained and lost.
fn file_row(file: &ChangedFile, selected: bool, cx: &App) -> Div {
    let muted = cx.theme().muted_foreground;
    let counts = if file.binary {
        vec![div().text_color(muted).child("binary")]
    } else {
        vec![
            div()
                .text_color(cx.theme().green)
                .child(format!("+{}", file.added)),
            div()
                .text_color(cx.theme().danger)
                .child(format!("−{}", file.removed)),
        ]
    };
    h_flex()
        .w_full()
        .h(ROW)
        .px_2()
        .gap_2()
        .items_center()
        .text_xs()
        .cursor_pointer()
        .when(selected, |row| row.bg(cx.theme().list_active))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .truncate()
                .font_family(MONO_FONT)
                .child(file.path.display().to_string()),
        )
        .when(file.created, |row| {
            row.child(div().text_color(muted).child("new"))
        })
        .children(counts)
}

/// A file's diff as git prints one, additions and removals on a faint
/// green or red.
fn patch(open: &Open, cx: &App) -> AnyElement {
    let text = match &open.diff {
        None => return note("Reading…", cx),
        Some(Err(why)) => return note(why, cx),
        Some(Ok(text)) => text,
    };
    let total = text.lines().count();
    patch_lines(text.lines().take(SHOWN_LINES), cx)
        .py_1()
        .when(total > SHOWN_LINES, |this| {
            this.child(
                div()
                    .px_2()
                    .text_color(cx.theme().muted_foreground)
                    .child(format!("… {} more lines", total - SHOWN_LINES)),
            )
        })
        .into_any_element()
}

/// Lines of a patch, additions and removals on a faint green or red.
fn patch_lines<'a>(lines: impl Iterator<Item = &'a str>, cx: &App) -> Div {
    let lines = lines.map(|line| {
        let (ground, colour) = match line.chars().next() {
            _ if line.starts_with("+++") || line.starts_with("---") => {
                (None, cx.theme().muted_foreground)
            }
            Some('+') => (Some(cx.theme().green.opacity(0.14)), cx.theme().foreground),
            Some('-') => (Some(cx.theme().danger.opacity(0.14)), cx.theme().foreground),
            Some('@') => (None, cx.theme().link),
            Some(' ') => (None, cx.theme().foreground),
            _ => (None, cx.theme().muted_foreground),
        };
        div()
            .px_2()
            .text_color(colour)
            .when_some(ground, |line, ground| line.bg(ground))
            .child(shown_line(line))
    });
    v_flex().text_xs().font_family(MONO_FONT).children(lines)
}

/// A patch line as drawn: tabs spread out, and cut after `LINE_CHARS`.
fn shown_line(line: &str) -> String {
    let mut shown: String = line.chars().take(LINE_CHARS).collect();
    if shown.len() < line.len() {
        shown.push('…');
    }
    shown.replace('\t', "    ")
}

/// A quiet line of text in place of a list or a diff.
fn note(text: &str, cx: &App) -> AnyElement {
    div()
        .p_2()
        .text_sm()
        .text_color(cx.theme().muted_foreground)
        .child(text.to_owned())
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::{LINE_CHARS, shown_line};

    #[test]
    fn a_long_patch_line_is_cut_and_a_short_one_kept() {
        assert_eq!(shown_line("+\tkept"), "+    kept");
        let long = format!("+{}", "é".repeat(5000));
        let shown = shown_line(&long);
        assert_eq!(shown.chars().count(), LINE_CHARS + 1);
        assert!(shown.ends_with('…'));
    }
}
