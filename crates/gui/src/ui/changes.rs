//! The Changes tab of the right panel, and what the status bar says of the
//! project: either what this session's tools changed, measured from the
//! copies rewind keeps, or the working tree as git sees it. A file's diff
//! is read when it is opened, off the UI thread.
//!
//! Nothing here polls. The list and the status are read again after a tool
//! runs, when the window comes back to the front, when the session or its
//! folder changes, and after a revert: the moments they can have moved.

use crate::prelude::*;
use gpui_component::Selectable;
use gpui_component::button::{ButtonGroup, ButtonVariant};
use gpui_component::dialog::DialogButtonProps;
use gpui_component::notification::Notification;
use letronna_core::git::{self, ChangedFile};
use letronna_core::history;

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
        LeaveChanges
    ]
);

/// Patch lines drawn under a file, past which the rest are counted: a
/// generated file's thousands of lines would each be an element.
const SHOWN_LINES: usize = 1500;

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
    /// The files in scope, or why there are none; `None` until read.
    files: Option<Result<Vec<ChangedFile>, String>>,
    selected: usize,
    open: Option<Open>,
    /// The project folder's status, with the folder it is of.
    status: Option<(PathBuf, git::Status)>,
    /// A read is running, and another is wanted once it is back.
    reading: bool,
    again: bool,
    focus: FocusHandle,
    scroll: UniformListScrollHandle,
}

struct Open {
    session: u64,
    scope: Scope,
    file: PathBuf,
    /// `None` while it is still being read.
    diff: Option<Result<String, String>>,
}

/// What one read brings back.
struct Read {
    files: Result<Vec<ChangedFile>, String>,
    status: Option<git::Status>,
}

impl Changes {
    pub(crate) fn new(cx: &mut App) -> Self {
        Self {
            scope: Scope::default(),
            read_for: None,
            files: None,
            selected: 0,
            open: None,
            status: None,
            reading: false,
            again: false,
            focus: cx.focus_handle(),
            scroll: UniformListScrollHandle::new(),
        }
    }
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
                    return Read {
                        files: Err("This session has no project folder.".into()),
                        status: None,
                    };
                };
                let files = match scope {
                    Scope::Session => Ok(history::session_files(session, &notes, &folder)),
                    Scope::Tree => git::changed_files(&folder),
                };
                Read {
                    files,
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
                    this.took_changes(read.files, cx);
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

    fn selected_change(&self) -> Option<&ChangedFile> {
        match &self.changes.files {
            Some(Ok(files)) => files.get(self.changes.selected),
            _ => None,
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
        self.changes.selected = 0;
        self.changes.open = None;
        self.refresh_changes(cx);
        cx.notify();
    }

    /// Move the selection, and the open diff with it, so what Revert and
    /// Add to message act on is always the file shown.
    fn move_selection(&mut self, by: isize, cx: &mut Context<Self>) {
        let count = match &self.changes.files {
            Some(Ok(files)) if !files.is_empty() => files.len(),
            _ => return,
        };
        let at = self
            .changes
            .selected
            .saturating_add_signed(by)
            .min(count - 1);
        self.changes.selected = at;
        self.changes
            .scroll
            .scroll_to_item(at, ScrollStrategy::Center);
        if self.changes.open.is_some() {
            self.open_selected(false, cx);
        }
        cx.notify();
    }

    /// Show the selected file's diff; `toggle` hides it instead when it is
    /// the one open.
    fn open_selected(&mut self, toggle: bool, cx: &mut Context<Self>) {
        let Some(file) = self.selected_change().map(|file| file.path.clone()) else {
            return;
        };
        let session = self.store.conversations[self.store.active].id;
        let scope = self.changes.scope;
        let open_now = self.changes.open.as_ref().is_some_and(|open| {
            open.session == session && open.scope == scope && open.file == file
        });
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
            diff: None,
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
            .as_ref()
            .filter(|open| open.session == conversation.id)
        else {
            return;
        };
        let Some(folder) = conversation.workspace.clone() else {
            return;
        };
        let (session, scope, file) = (open.session, open.scope, open.file.clone());
        let notes = file_notes(&conversation.messages);
        let asked = file.clone();
        let diff = cx.background_executor().spawn(async move {
            match scope {
                Scope::Session => history::session_patch(session, &notes, &folder, &asked),
                Scope::Tree => git::working_diff(&folder, Some(&[asked])).map(|diff| {
                    if diff.is_empty() {
                        "No change since the last commit.".to_owned()
                    } else {
                        diff.patch
                    }
                }),
            }
        });
        cx.spawn(async move |this, cx| {
            let diff = diff.await;
            let _ = this.update(cx, |this, cx| {
                // Only for the file still open: a later choice wins.
                if let Some(open) = this.changes.open.as_mut().filter(|open| {
                    open.session == session && open.scope == scope && open.file == file
                }) {
                    open.diff = Some(diff);
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
        let Some(file) = self.selected_change().map(|file| file.path.clone()) else {
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
        let body = match &self.changes.files {
            None => note("Reading…", cx),
            Some(Err(why)) => note(why, cx),
            Some(Ok(files)) if files.is_empty() => note(
                match scope {
                    Scope::Session => "This session has changed no files in its project.",
                    Scope::Tree => "Nothing has changed since the last commit.",
                },
                cx,
            ),
            Some(Ok(files)) => {
                let open = self
                    .changes
                    .open
                    .as_ref()
                    .filter(|open| open.session == conversation.id && open.scope == scope);
                v_flex()
                    .flex_1()
                    .min_h_0()
                    .child(self.file_list(files.len(), cx))
                    .children(open.map(|open| self.open_file(open, cx)))
                    .into_any_element()
            }
        };
        v_flex()
            .id("changes")
            .size_full()
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
            .child(header)
            .child(body)
            .into_any_element()
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
    fn open_file(&self, open: &Open, cx: &mut Context<Self>) -> AnyElement {
        let session_scope = open.scope == Scope::Session;
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
                    .child(patch(open, cx)),
            )
            .into_any_element()
    }
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
    let lines = text.lines().take(SHOWN_LINES).map(|line| {
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
            .child(line.replace('\t', "    "))
    });
    v_flex()
        .py_1()
        .text_xs()
        .font_family(MONO_FONT)
        .children(lines)
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

/// A quiet line of text in place of a list or a diff.
fn note(text: &str, cx: &App) -> AnyElement {
    div()
        .p_2()
        .text_sm()
        .text_color(cx.theme().muted_foreground)
        .child(text.to_owned())
        .into_any_element()
}
