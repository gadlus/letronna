//! The Changes tab of the right panel: the files this session's tools wrote,
//! and a file's diff since the last commit when it is clicked. Git is asked
//! only on a click, off the UI thread, so a draw costs nothing but the list.

use crate::prelude::*;
use letronna_core::history;

/// Patch lines drawn under a file, past which the rest are counted: a
/// generated file's thousands of lines would each be an element.
const SHOWN_LINES: usize = 1500;

/// The file whose diff is open, and what git said of it.
#[derive(Default)]
pub(crate) struct Changes {
    open: Option<Open>,
    /// Clicks that asked git so far, to tell each answer's click apart.
    asked: u64,
}

struct Open {
    session: u64,
    file: PathBuf,
    /// Which click this is, so an answer to an earlier one is dropped.
    ask: u64,
    /// `None` while git is still being asked.
    diff: Option<Result<String, String>>,
}

impl Chat {
    /// Whether the session on screen has written any file, for the tab to
    /// show; the list itself waits until the tab is drawn.
    pub(crate) fn has_changes(&self) -> bool {
        self.store.conversations[self.store.active]
            .messages
            .iter()
            .any(|m| m.role == "tool" && !m.files.is_empty())
    }

    /// Show a file's diff, or hide it when it is the one open.
    fn open_change(&mut self, folder: PathBuf, file: PathBuf, cx: &mut Context<Self>) {
        let session = self.store.conversations[self.store.active].id;
        if self
            .changes
            .open
            .as_ref()
            .is_some_and(|open| open.session == session && open.file == file)
        {
            self.changes.open = None;
            cx.notify();
            return;
        }
        self.changes.asked += 1;
        let ask = self.changes.asked;
        self.changes.open = Some(Open {
            session,
            file: file.clone(),
            ask,
            diff: None,
        });
        let only = vec![file];
        cx.spawn(async move |this, cx| {
            let diff = cx
                .background_executor()
                .spawn(async move {
                    letronna_core::git::working_diff(&folder, Some(&only)).map(|diff| {
                        if diff.is_empty() {
                            "No change since the last commit.".to_owned()
                        } else {
                            diff.patch
                        }
                    })
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                // Only for this click: a later one, in this session or
                // another with its own project, wins.
                if let Some(open) = this.changes.open.as_mut().filter(|o| o.ask == ask) {
                    open.diff = Some(diff);
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    /// The tab's body: each file the session changed in its project, and
    /// under the one clicked, its diff.
    pub(crate) fn changes_tab(&self, cx: &mut Context<Self>) -> AnyElement {
        let conversation = &self.store.conversations[self.store.active];
        let Some(folder) = conversation.workspace.clone() else {
            return note("This session has no project folder.", cx);
        };
        let files = history::written(&conversation.messages, &folder);
        if files.is_empty() {
            return note("This session has changed no files in its project.", cx);
        }
        let open = self
            .changes
            .open
            .as_ref()
            .filter(|open| open.session == conversation.id);
        let rows = files.into_iter().enumerate().map(|(at, file)| {
            let shown = open.filter(|open| open.file == file);
            let (folder, clicked) = (folder.clone(), file.clone());
            v_flex()
                .child(
                    Button::new(("change", at))
                        .plain(cx)
                        .xsmall()
                        .justify_start()
                        .font_family(MONO_FONT)
                        .label(file.display().to_string())
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.open_change(folder.clone(), clicked.clone(), cx)
                        })),
                )
                .when_some(shown, |row, open| row.child(patch(open, cx)))
        });
        v_flex()
            .id("changes")
            .size_full()
            .overflow_y_scroll()
            .p_2()
            .gap_1()
            .children(rows)
            .into_any_element()
    }
}

/// A file's diff, each line coloured by what it does.
fn patch(open: &Open, cx: &App) -> AnyElement {
    let text = match &open.diff {
        None => return note("Reading…", cx),
        Some(Err(why)) => return note(why, cx),
        Some(Ok(text)) => text,
    };
    let total = text.lines().count();
    let lines = text.lines().take(SHOWN_LINES).map(|line| {
        let colour = match line.chars().next() {
            _ if line.starts_with("+++") || line.starts_with("---") => cx.theme().muted_foreground,
            Some('+') => cx.theme().green,
            Some('-') => cx.theme().danger,
            Some('@') => cx.theme().link,
            Some(' ') => cx.theme().foreground,
            _ => cx.theme().muted_foreground,
        };
        div().text_color(colour).child(line.replace('\t', "    "))
    });
    v_flex()
        .px_2()
        .pb_2()
        .text_xs()
        .font_family(MONO_FONT)
        .children(lines)
        .when(total > SHOWN_LINES, |this| {
            this.child(
                div()
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
