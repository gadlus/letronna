//! The picker the sessions menu opens to continue a Claude Code or Codex
//! session here: the newest from both tools, narrowed by typing, one of
//! them brought in as a session of this window's own.
//!
//! The list is read once, off the window's thread, when the picker opens,
//! and the picked file is read the same way; between the two nothing runs,
//! so an open picker costs nothing while it waits for a key.

use crate::prelude::*;
use letronna_core::import::{self, Candidate};

actions!(import, [PickPrevious, PickNext, PickThis]);

/// Sessions offered at most: more than a filter ever needs to reach, and
/// few enough that listing reads only so many file heads.
const CHOICES: usize = 200;
/// A row's height, which `uniform_list` lays every row out at.
const ROW: Pixels = px(44.);
/// Rows shown before the list scrolls.
const ROWS_SHOWN: f32 = 8.;

pub(crate) struct ImportPicker {
    chat: WeakEntity<Chat>,
    query: Entity<InputState>,
    /// Every session found, newest first; `None` while they are being read.
    found: Option<Vec<Candidate>>,
    /// Which of `found` match what is typed, in the same order.
    shown: Vec<usize>,
    /// The row Enter picks, an index into `shown`.
    selected: usize,
    scroll: UniformListScrollHandle,
    /// Set while the picked file is read, so a second Enter does not
    /// import it twice.
    importing: bool,
    /// Why the last pick could not be brought in.
    error: Option<String>,
    _listing: Task<()>,
    _query: Subscription,
}

impl ImportPicker {
    fn new(chat: WeakEntity<Chat>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let query = cx.new(|cx| InputState::new(window, cx).placeholder("Filter sessions"));
        let _query = cx.subscribe_in(&query, window, |this, _, event: &InputEvent, _, cx| {
            if matches!(event, InputEvent::Change) {
                this.narrow(cx);
            }
        });
        let _listing = cx.spawn(async move |this, cx| {
            let found = cx
                .background_executor()
                .spawn(async { import::list(CHOICES) })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.found = Some(found);
                this.narrow(cx);
            });
        });
        Self {
            chat,
            query,
            found: None,
            shown: Vec::new(),
            selected: 0,
            scroll: UniformListScrollHandle::new(),
            importing: false,
            error: None,
            _listing,
            _query,
        }
    }

    /// Show only the sessions matching what is typed, from the first.
    fn narrow(&mut self, cx: &mut Context<Self>) {
        let query = self.query.read(cx).value().to_lowercase();
        self.shown = self
            .found
            .iter()
            .flatten()
            .enumerate()
            .filter(|(_, c)| matches(c, &query))
            .map(|(i, _)| i)
            .collect();
        self.selected = 0;
        self.scroll.scroll_to_item(0, ScrollStrategy::Top);
        cx.notify();
    }

    fn step(&mut self, down: bool, cx: &mut Context<Self>) {
        let Some(last) = self.shown.len().checked_sub(1) else {
            return;
        };
        self.selected = if down {
            (self.selected + 1).min(last)
        } else {
            self.selected.saturating_sub(1)
        };
        self.scroll
            .scroll_to_item(self.selected, ScrollStrategy::Top);
        cx.notify();
    }

    /// Bring in the session at row `row` and open it, or open the session
    /// an earlier import of it made.
    fn pick(&mut self, row: usize, window: &mut Window, cx: &mut Context<Self>) {
        if self.importing {
            return;
        }
        let Some(candidate) = self
            .shown
            .get(row)
            .and_then(|&i| self.found.as_ref()?.get(i))
            .cloned()
        else {
            return;
        };
        let Some(chat) = self.chat.upgrade() else {
            return;
        };
        let earlier = chat
            .read(cx)
            .store
            .find_by_import(candidate.tool, &candidate.session_id);
        if let Some(index) = earlier {
            window.close_dialog(cx);
            chat.update(cx, |chat, cx| chat.open_imported(index, window, cx));
            return;
        }
        self.selected = row;
        self.importing = true;
        self.error = None;
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let imported = cx
                .background_executor()
                .spawn(async move { import::import(&candidate) })
                .await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.importing = false;
                let imported = match imported {
                    Ok(imported) => imported,
                    Err(error) => {
                        this.error = Some(error);
                        cx.notify();
                        return;
                    }
                };
                let skipped = imported.skipped;
                window.close_dialog(cx);
                chat.update(cx, |chat, cx| {
                    let id = chat.store.reserve_id();
                    chat.store
                        .conversations
                        .push(import::into_conversation(imported, id));
                    let index = chat.store.conversations.len() - 1;
                    chat.open_imported(index, window, cx);
                });
                // The picker is gone by now, so this is said where the
                // window says the rest of what finished out of sight.
                if skipped > 0 {
                    window.push_notification(
                        gpui_component::notification::Notification::warning(skipped_note(skipped)),
                        cx,
                    );
                }
            });
        })
        .detach();
    }

    fn row(&self, row: usize, now: u64, cx: &mut Context<Self>) -> Option<Stateful<Div>> {
        let candidate = self.found.as_ref()?.get(*self.shown.get(row)?)?;
        let selected = row == self.selected;
        let about = format!(
            "{}  ·  {}  ·  {}",
            candidate.tool.label(),
            folder(candidate),
            age_label(candidate.updated, now)
        );
        Some(
            v_flex()
                .id(("import", row))
                .h(ROW)
                .px_2()
                .justify_center()
                .rounded(cx.theme().radius)
                .cursor_pointer()
                .when(selected, |this| this.bg(cx.theme().accent))
                .active(|s| s.bg(cx.theme().accent.opacity(0.6)))
                .child(div().text_sm().truncate().child(title(candidate)))
                .child(
                    div()
                        .text_xs()
                        .truncate()
                        .text_color(cx.theme().muted_foreground)
                        .child(about),
                )
                .on_click(cx.listener(move |this, _, window, cx| this.pick(row, window, cx))),
        )
    }

    /// One line in place of the list: while it is read, when there is
    /// nothing to list, or when nothing matches.
    fn note(&self, cx: &Context<Self>) -> Option<Div> {
        let text = match &self.found {
            None => "Looking for sessions…",
            Some(found) if found.is_empty() => {
                "Neither Claude Code nor Codex has any sessions on this machine."
            }
            Some(_) if self.shown.is_empty() => "No session matches.",
            Some(_) => return None,
        };
        Some(
            div()
                .px_2()
                .py_2()
                .text_sm()
                .text_color(cx.theme().muted_foreground)
                .child(text),
        )
    }
}

impl Render for ImportPicker {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let now = backend::now();
        let rows = self.shown.len();
        let list = uniform_list(
            "import-sessions",
            rows,
            cx.processor(move |this, range: std::ops::Range<usize>, _, cx| {
                range.filter_map(|row| this.row(row, now, cx)).collect()
            }),
        )
        .track_scroll(self.scroll.clone())
        .h(ROW * (rows as f32).min(ROWS_SHOWN));
        v_flex()
            .key_context("ImportPicker")
            .gap_2()
            .on_action(cx.listener(|this, _: &PickPrevious, _, cx| this.step(false, cx)))
            .on_action(cx.listener(|this, _: &PickNext, _, cx| this.step(true, cx)))
            .on_action(
                cx.listener(|this, _: &PickThis, window, cx| this.pick(this.selected, window, cx)),
            )
            .child(Input::new(&self.query))
            .children(self.note(cx))
            .when(rows > 0, |this| this.child(list))
            .when(self.importing, |this| {
                this.child(
                    h_flex()
                        .px_2()
                        .gap_2()
                        .text_sm()
                        .text_color(cx.theme().muted_foreground)
                        .child(gpui_component::spinner::Spinner::new().xsmall())
                        .child("Importing…"),
                )
            })
            .when_some(self.error.clone(), |this, error| {
                this.child(
                    div()
                        .px_2()
                        .text_sm()
                        .text_color(cx.theme().danger)
                        .child(error),
                )
            })
    }
}

impl Chat {
    /// Open the picker of Claude Code and Codex sessions to continue here.
    pub(crate) fn open_import(&mut self, _: usize, window: &mut Window, cx: &mut Context<Self>) {
        let chat = cx.entity().downgrade();
        let picker = cx.new(|cx| ImportPicker::new(chat, window, cx));
        let content = picker.clone();
        window.open_dialog(cx, move |dialog, _, _| {
            dialog
                .title(
                    div()
                        .font_family(HEADING_FONT)
                        .child("Continue from Claude Code or Codex"),
                )
                .w(px(560.))
                .child(content.clone())
        });
        // After opening, which takes focus for the dialog itself; typing
        // filters, so the cursor belongs in the box.
        let query = picker.read(cx).query.clone();
        query.update(cx, |input, cx| input.focus(window, cx));
    }

    /// Show the session at `index` as the current one, as picking it in the
    /// sidebar does.
    fn open_imported(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        self.store.active = index;
        self.error = None;
        self.page = Page::Chat;
        self.follow();
        self.focus_composer(window, cx);
        self.save();
        cx.notify();
    }
}

/// Whether `candidate` holds every word of `query`, already lowercase, in
/// its title, its tool's name or its project folder, each as its row shows it.
fn matches(candidate: &Candidate, query: &str) -> bool {
    let haystack = format!(
        "{} {} {}",
        title(candidate),
        candidate.tool.label(),
        folder(candidate)
    )
    .to_lowercase();
    query.split_whitespace().all(|word| haystack.contains(word))
}

fn title(candidate: &Candidate) -> String {
    match candidate.title.trim() {
        "" => "Untitled".to_owned(),
        title => title.to_owned(),
    }
}

/// The last part of the session's project folder, which is how a project
/// is known at a glance.
fn folder(candidate: &Candidate) -> String {
    candidate
        .workspace
        .as_ref()
        .and_then(|p| p.file_name())
        .map_or_else(
            || "no project".to_owned(),
            |n| n.to_string_lossy().into_owned(),
        )
}

fn skipped_note(skipped: usize) -> String {
    match skipped {
        1 => "1 part of the original session could not be imported.".to_owned(),
        n => format!("{n} parts of the original session could not be imported."),
    }
}

#[cfg(test)]
mod tests {
    use super::{folder, matches, skipped_note, title};
    use letronna_core::import::{Candidate, Tool};
    use std::path::PathBuf;

    fn candidate(tool: Tool, title: &str, workspace: Option<&str>) -> Candidate {
        Candidate {
            tool,
            session_id: "s1".into(),
            path: PathBuf::from("s1.jsonl"),
            title: title.into(),
            workspace: workspace.map(PathBuf::from),
            updated: 0,
        }
    }

    #[test]
    fn typing_narrows_by_title_tool_and_folder_ignoring_case_and_order() {
        let c = candidate(Tool::Codex, "Fix the Login bug", Some("/home/dev/webshop"));
        assert!(matches(&c, ""));
        assert!(matches(&c, "login"));
        assert!(matches(&c, "codex webshop"));
        assert!(matches(&c, "webshop fix"));
        assert!(!matches(&c, "claude"));
        assert!(!matches(&c, "login payments"));
    }

    #[test]
    fn a_session_without_a_title_or_project_still_reads_as_something() {
        let c = candidate(Tool::ClaudeCode, "  ", None);
        assert_eq!(title(&c), "Untitled");
        assert_eq!(folder(&c), "no project");
        assert!(matches(&c, "no project"));
        assert!(
            matches(&c, "untitled"),
            "what the row shows is what is searched"
        );
    }

    #[test]
    fn the_skipped_note_counts_in_words_that_agree() {
        assert_eq!(
            skipped_note(1),
            "1 part of the original session could not be imported."
        );
        assert_eq!(
            skipped_note(3),
            "3 parts of the original session could not be imported."
        );
    }
}
