//! The Artifacts page: files the agent has written.
//!
//! Nothing extra is stored for this. Every write already leaves a `tool`
//! message in the session it happened in, so the list is derived from the
//! history on the way in. That means it can never disagree with what actually
//! happened, and deleting a session takes its artifacts with it. The history
//! is read from disk, by `store::artifacts`, as most sessions are not in
//! memory: once each time the page opens, off the UI thread.

use crate::prelude::*;

impl Chat {
    /// Read the files the Artifacts page lists again, off the UI thread:
    /// every session with a project folder is looked through.
    pub(crate) fn read_artifacts(&mut self, cx: &mut Context<Self>) {
        self.artifacts = None;
        cx.spawn(async move |this, cx| {
            let found = cx
                .background_executor()
                .spawn(async { letronna_core::store::artifacts() })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.artifacts = Some(found);
                cx.notify();
            });
        })
        .detach();
    }

    pub(crate) fn artifacts_page(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let artifacts = self.artifacts.as_deref().unwrap_or_default();
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
                    .py_3()
                    .border_b_1()
                    .border_color(cx.theme().border)
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
                                    .plain(cx)
                                    .xsmall()
                                    .label("Open")
                                    .disabled(!exists)
                                    .on_click(move |_, _, cx| cx.open_with_system(&open_path)),
                            )
                            .child(
                                Button::new(("reveal", index))
                                    .plain(cx)
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
                            // Not "nothing" while the sessions are still being read.
                            .child(if self.artifacts.as_ref().is_some_and(Vec::is_empty) {
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
                    .child(v_flex().children(rows)),
            )
    }
}
