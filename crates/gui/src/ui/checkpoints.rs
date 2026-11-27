//! The Checkpoints tab: every turn of the session on screen that changed
//! files, newest first, each a point the project's files can be put back
//! to. Restoring asks once more in place, then puts back every file the
//! turns from there on wrote, as a rewind would, but keeps the conversation
//! and tells the model which files went back.

use crate::prelude::*;
use letronna_core::history;

/// One turn that wrote files: where it starts, what was asked, and the
/// files it wrote, relative to the project.
pub(crate) struct Checkpoint {
    pub(crate) first: usize,
    pub(crate) asked: String,
    pub(crate) files: Vec<std::path::PathBuf>,
}

/// The turns of `messages` that wrote files inside `workspace`, newest
/// first. A turn starts at a message of yours and runs to the next.
pub(crate) fn checkpoints(
    messages: &[letronna_core::chat::Message],
    workspace: &std::path::Path,
) -> Vec<Checkpoint> {
    let starts: Vec<usize> = (messages.iter().enumerate())
        .filter(|(_, m)| m.role == "user" && !m.notice)
        .map(|(i, _)| i)
        .collect();
    let mut out = Vec::new();
    for (n, &first) in starts.iter().enumerate() {
        let end = starts.get(n + 1).copied().unwrap_or(messages.len());
        let files = history::written(&messages[first..end], workspace);
        if files.is_empty() {
            continue;
        }
        let asked = messages[first]
            .content
            .lines()
            .next()
            .unwrap_or_default()
            .to_owned();
        out.push(Checkpoint {
            first,
            asked,
            files,
        });
    }
    out.reverse();
    out
}

impl Chat {
    pub(crate) fn has_checkpoints(&self) -> bool {
        let open = &self.store.conversations[self.store.active];
        open.workspace
            .as_deref()
            .is_some_and(|ws| !checkpoints(&open.messages, ws).is_empty())
    }

    /// Put the project's files back as they were before the turn starting
    /// at `first`: every file the turns from there on wrote. The
    /// conversation stays; the model is told what went back.
    fn restore_checkpoint(&mut self, first: usize, window: &mut Window, cx: &mut Context<Self>) {
        let open = &self.store.conversations[self.store.active];
        let session = open.id;
        let plan = history::plan(session, &open.messages[first..], open.workspace.as_deref());
        self.checkpoint_asking = None;
        if plan.is_empty() {
            self.toast(
                "Checkpoints: The files are already as they were then.",
                crate::ui::toast::Kind::Info,
                cx,
            );
            return;
        }
        // Held while the files go back and the model is told.
        let held = match self.hold_open("restore it once it is done") {
            Ok(held) => held,
            Err(why) => return self.toast(why, crate::ui::toast::Kind::Warning, cx),
        };
        self.put_files_back(session, plan, true, held, window, cx);
    }

    pub(crate) fn checkpoints_tab(&self, cx: &mut Context<Self>) -> AnyElement {
        let open = &self.store.conversations[self.store.active];
        let points = open
            .workspace
            .as_deref()
            .map(|ws| checkpoints(&open.messages, ws))
            .unwrap_or_default();
        let theme = cx.theme();
        let (muted, border) = (theme.muted_foreground, theme.border);
        let busy = !self.idle();
        let mut list = v_flex().gap_2().p_3();
        for (n, point) in points.iter().enumerate() {
            let first = point.first;
            let asking = self.checkpoint_asking == Some(first);
            let names: Vec<String> = (point.files.iter().take(3))
                .map(|p| {
                    p.file_name()
                        .map(|f| f.to_string_lossy().into_owned())
                        .unwrap_or_default()
                })
                .collect();
            let more = point.files.len().saturating_sub(3);
            let files = match more {
                0 => names.join(", "),
                n => format!("{} and {n} more", names.join(", ")),
            };
            let action: AnyElement = if asking {
                h_flex()
                    .gap_1()
                    .child(
                        Button::new(("checkpoint-no", first))
                            .ghost()
                            .xsmall()
                            .label("Cancel")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.checkpoint_asking = None;
                                cx.notify();
                            })),
                    )
                    .child(
                        Button::new(("checkpoint-yes", first))
                            .danger()
                            .xsmall()
                            .label("Restore files")
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.restore_checkpoint(first, window, cx)
                            })),
                    )
                    .into_any_element()
            } else {
                Button::new(("checkpoint", first))
                    .framed(cx)
                    .xsmall()
                    .icon(Icon::new(AppIcon::Reload))
                    .label("Restore to before this")
                    .disabled(busy)
                    .tooltip(if busy {
                        "Wait for the turn to finish"
                    } else {
                        "Put the project's files back as they were before this turn"
                    })
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.checkpoint_asking = Some(first);
                        cx.notify();
                    }))
                    .into_any_element()
            };
            list = list.child(
                v_flex()
                    .gap_2()
                    .px_4()
                    .py_3()
                    .rounded(theme.radius_lg)
                    .border_1()
                    .border_color(if asking { theme.danger } else { border })
                    .bg(theme.secondary)
                    .child(
                        h_flex()
                            .gap_2()
                            .items_center()
                            .child(div().size(px(8.)).rounded_full().flex_shrink_0().bg(
                                if n == 0 {
                                    theme.link
                                } else {
                                    muted.opacity(0.5)
                                },
                            ))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .truncate()
                                    .text_sm()
                                    .font_weight(FontWeight::MEDIUM)
                                    .child(if point.asked.is_empty() {
                                        "Your message".to_owned()
                                    } else {
                                        point.asked.clone()
                                    }),
                            ),
                    )
                    .child(div().text_xs().text_color(muted).child(format!(
                        "{} · {files}",
                        match point.files.len() {
                            1 => "1 file".to_owned(),
                            n => format!("{n} files"),
                        }
                    )))
                    .when(asking, |this| {
                        this.child(div().text_xs().text_color(theme.danger).child(
                            "Every file changed from this turn on goes back as it was. \
                             The conversation stays.",
                        ))
                    })
                    .child(h_flex().justify_end().child(action)),
            );
        }
        let empty = points.is_empty().then(|| {
            v_flex()
                .items_center()
                .gap_2()
                .p_8()
                .child(Icon::new(AppIcon::Clock).size_6().text_color(muted))
                .child(
                    div()
                        .text_sm()
                        .text_color(muted)
                        .text_center()
                        .child("Each turn that changes files becomes a point to go back to."),
                )
        });
        v_flex()
            .size_full()
            .child(
                div()
                    .px_4()
                    .py_3()
                    .border_b_1()
                    .border_color(border)
                    .text_sm()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child("Checkpoints"),
            )
            .child(
                div()
                    .id("checkpoints-list")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .child(list)
                    .children(empty),
            )
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::checkpoints;
    use letronna_core::chat::Message;
    use letronna_core::history::FileChange;
    use std::path::{Path, PathBuf};

    fn you(text: &str) -> Message {
        Message {
            role: "user".into(),
            content: text.into(),
            ..Default::default()
        }
    }

    fn wrote(path: &str) -> Message {
        Message {
            role: "tool".into(),
            name: "edit_file".into(),
            files: vec![FileChange {
                path: PathBuf::from(path),
                after: "h".into(),
                ..Default::default()
            }],
            ..Default::default()
        }
    }

    #[test]
    fn only_turns_that_wrote_files_are_points_and_the_newest_comes_first() {
        let ws = Path::new("/p");
        let messages = vec![
            you("Fix the bug\nin the list"),
            wrote("/p/a.rs"),
            you("What does it do?"),
            you("Add tests"),
            wrote("/p/b.rs"),
            wrote("/p/c.rs"),
        ];
        let points = checkpoints(&messages, ws);
        assert_eq!(points.len(), 2);
        assert_eq!((points[0].first, points[0].files.len()), (3, 2));
        assert_eq!(points[1].asked, "Fix the bug");
    }
}
