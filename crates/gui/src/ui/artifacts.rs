//! The Artifacts page: files the agent has written.
//!
//! Nothing extra is stored for this. Every write already leaves a `tool`
//! message in the session it happened in, so the list is derived from the
//! history on the way in. That means it can never disagree with what actually
//! happened, and deleting a session takes its artifacts with it.

use letronna_core::tools;
use crate::prelude::*;

pub(crate) struct Artifact {
    pub(crate) path: PathBuf,
    /// The session it was written in.
    pub(crate) session: String,
    pub(crate) written: u64,
    /// How many times it was written or edited across all sessions.
    pub(crate) writes: usize,
}

/// Newest first, one entry per file however often it was touched.
pub(crate) fn collect(store: &Store) -> Vec<Artifact> {
    let mut found: Vec<Artifact> = Vec::new();
    for conversation in &store.conversations {
        let Some(workspace) = &conversation.workspace else {
            continue;
        };
        for message in &conversation.messages {
            if message.role != "tool" || message.is_error {
                continue;
            }
            if !matches!(message.name.as_str(), "write_file" | "edit_file") {
                continue;
            }
            let Ok(args) = tools::parse_args(&message.args) else {
                continue;
            };
            let Some(relative) = args["path"].as_str() else {
                continue;
            };
            let path = workspace.join(relative);
            match found.iter_mut().find(|a| a.path == path) {
                Some(existing) => {
                    existing.writes += 1;
                    if conversation.updated >= existing.written {
                        existing.written = conversation.updated;
                        existing.session = conversation.title.clone();
                    }
                }
                None => found.push(Artifact {
                    path,
                    session: conversation.title.clone(),
                    written: conversation.updated,
                    writes: 1,
                }),
            }
        }
    }
    found.sort_by_key(|a| std::cmp::Reverse(a.written));
    found
}

impl Chat {
    pub(crate) fn artifacts_page(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let artifacts = collect(&self.store);
        let now = backend::now();
        let rows: Vec<AnyElement> = artifacts
            .iter()
            .enumerate()
            .map(|(index, artifact)| {
                let name = artifact
                    .path
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| artifact.path.display().to_string());
                let folder = artifact
                    .path
                    .parent()
                    .map(|p| p.display().to_string())
                    .unwrap_or_default();
                // A file can be moved or deleted outside Letronna, so say so
                // rather than offering to open something that is not there.
                let exists = artifact.path.exists();
                let open_path = artifact.path.clone();
                let reveal_path = artifact.path.clone();
                h_flex()
                    .w_full()
                    .items_center()
                    .justify_between()
                    .gap_3()
                    .p_3()
                    .rounded(cx.theme().radius)
                    .bg(cx.theme().secondary.opacity(0.3))
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .gap_0p5()
                            .child(
                                h_flex()
                                    .gap_2()
                                    .items_center()
                                    .child(
                                        div()
                                            .text_sm()
                                            .font_weight(FontWeight::MEDIUM)
                                            .truncate()
                                            .child(name),
                                    )
                                    .when(!exists, |this| {
                                        this.child(
                                            div()
                                                .text_xs()
                                                .text_color(cx.theme().muted_foreground)
                                                .child("missing"),
                                        )
                                    }),
                            )
                            .child(
                                h_flex()
                                    .gap_2()
                                    .min_w_0()
                                    .text_xs()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(div().truncate().child(folder))
                                    .child(format!("· {}", artifact.session))
                                    .when(artifact.writes > 1, |this| {
                                        this.child(format!("· {} writes", artifact.writes))
                                    })
                                    .child(format!("· {}", age_label(artifact.written, now))),
                            ),
                    )
                    .child(
                        h_flex()
                            .gap_1()
                            .items_center()
                            .child(
                                Button::new(("open", index))
                                    .ghost()
                                    .xsmall()
                                    .label("Open")
                                    .disabled(!exists)
                                    .on_click(move |_, _, cx| cx.open_with_system(&open_path)),
                            )
                            .child(
                                Button::new(("reveal", index))
                                    .ghost()
                                    .xsmall()
                                    .icon(Icon::new(AppIcon::Folder))
                                    .tooltip("Show in folder")
                                    .disabled(!exists)
                                    .on_click(move |_, _, cx| cx.reveal_path(&reveal_path)),
                            ),
                    )
                    .into_any_element()
            })
            .collect();

        v_flex()
            .flex_1()
            .min_h_0()
            .px_8()
            .py_6()
            .gap_4()
            .child(
                v_flex()
                    .gap_1()
                    .child(
                        div()
                            .text_xl()
                            .font_family(HEADING_FONT)
                            .font_weight(FontWeight::SEMIBOLD)
                            .child("Artifacts"),
                    )
                    .child(
                        div()
                            .text_sm()
                            .text_color(cx.theme().muted_foreground)
                            .child(if rows.is_empty() {
                                "Files Letronna writes will be listed here, newest first. \
                                 Nothing yet."
                            } else {
                                "Files Letronna has written, newest first. The list comes \
                                 from the sessions themselves, so removing a session removes \
                                 its files from here too."
                            }),
                    ),
            )
            .child(
                div()
                    .id("artifact-list")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .child(v_flex().gap_2().children(rows)),
            )
    }
}

#[cfg(test)]
mod tests {
    // Not `use super::*`: the prelude re-exports gpui, whose `test` macro
    // would shadow the built-in attribute.
    use super::collect;
    use letronna_core::chat::Message;
    use letronna_core::store::{Conversation, Store};

    fn wrote(path: &str, name: &str, is_error: bool) -> Message {
        Message {
            role: "tool".into(),
            name: name.into(),
            args: format!(r#"{{"path":"{path}"}}"#),
            is_error,
            ..Default::default()
        }
    }

    #[test]
    fn collects_writes_and_counts_repeats() {
        let store = Store {
            conversations: vec![
                Conversation {
                    title: "older".into(),
                    updated: 100,
                    workspace: Some("/work".into()),
                    messages: vec![
                        wrote("a.txt", "write_file", false),
                        wrote("b.txt", "write_file", false),
                        // Failures never produced a file.
                        wrote("never.txt", "write_file", true),
                        // Reads are not artifacts.
                        wrote("c.txt", "read_file", false),
                    ],
                    ..Default::default()
                },
                Conversation {
                    title: "newer".into(),
                    updated: 200,
                    workspace: Some("/work".into()),
                    messages: vec![wrote("a.txt", "edit_file", false)],
                    ..Default::default()
                },
                // No project folder means no path to resolve against.
                Conversation {
                    title: "chat".into(),
                    updated: 300,
                    messages: vec![wrote("d.txt", "write_file", false)],
                    ..Default::default()
                },
            ],
            ..Default::default()
        };

        let found = collect(&store);
        let names: Vec<String> = found
            .iter()
            .map(|a| a.path.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, ["a.txt", "b.txt"]);

        // a.txt was written then edited, and reports the newer session.
        assert_eq!(found[0].writes, 2);
        assert_eq!(found[0].session, "newer");
        assert_eq!(found[1].writes, 1);
    }
}
