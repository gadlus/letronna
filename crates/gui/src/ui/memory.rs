//! The Memory page of settings: what Letronna keeps from one session to the
//! next, entry by entry under each store's budget, and SOUL.md.
//!
//! Its own entity rather than more fields on Chat, so its state and its
//! reads of the files stay here. The files are read when settings open and
//! after each change, never while painting or idle, and every write goes
//! through core's memory API, off the UI thread: under the store lock, by
//! the memory tool's rules, and pinned to the text the person was shown.
//! What is typed and not saved stays in the page until it is saved or
//! cancelled, whatever is read meanwhile.

use crate::prelude::*;
use gpui_component::progress::Progress;
use letronna_core::memory;

actions!(memory, [Save]);

pub(crate) struct MemoryPage {
    /// The project folder of the session settings were opened over, whose
    /// notes are shown.
    workspace: Option<PathBuf>,
    stores: Vec<memory::Store>,
    /// Each store's entries as they are drawn, worked out when read.
    shown: Vec<Vec<Shown>>,
    soul: Option<memory::Store>,
    /// The entry being edited: its store, number and text as shown.
    editing: Option<(&'static str, usize, String)>,
    entry: Entity<InputState>,
    soul_text: Entity<InputState>,
    /// SOUL.md as it was last put in its box, which a save is pinned to.
    soul_loaded: String,
    /// Whether the box differs from it, as last drawn.
    soul_dirty: bool,
    /// What the last change said, and whether it was refused.
    notice: Option<(String, bool)>,
}

/// An entry as the page draws it.
struct Shown {
    /// The text without what would hide or reorder it.
    text: String,
    /// What the scanner found in it, in words that never repeat it.
    flag: Option<String>,
}

impl MemoryPage {
    pub(crate) fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let entry = cx.new(|cx| {
            InputState::new(window, cx)
                .multi_line(true)
                .auto_grow(1, 10)
        });
        let soul_text = cx.new(|cx| {
            InputState::new(window, cx)
                .multi_line(true)
                .auto_grow(4, 16)
                .placeholder("You are Letronna, a careful assistant who…")
        });
        // "Not saved" and Cancel show once the box differs from the file,
        // redrawn only when that flips, not on every key.
        cx.subscribe(&soul_text, |this, input, event: &InputEvent, cx| {
            if !matches!(event, InputEvent::Change) {
                return;
            }
            let dirty = unsaved(&input.read(cx).value(), &this.soul_loaded);
            if dirty != this.soul_dirty {
                this.soul_dirty = dirty;
                cx.notify();
            }
        })
        .detach();
        Self {
            workspace: None,
            stores: Vec::new(),
            shown: Vec::new(),
            soul: None,
            editing: None,
            entry,
            soul_text,
            soul_loaded: String::new(),
            soul_dirty: false,
            notice: None,
        }
    }

    /// Read the stores afresh, with `workspace`'s notes. An entry being
    /// edited and SOUL.md's unsaved text are kept.
    pub(crate) fn refresh(
        &mut self,
        workspace: Option<PathBuf>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Another project's notes are another store, not this one's entry.
        if workspace != self.workspace
            && self
                .editing
                .as_ref()
                .is_some_and(|(target, ..)| *target == "project")
        {
            self.editing = None;
        }
        self.workspace = workspace;
        self.notice = None;
        self.reload(window, cx);
    }

    /// Read the stores again, leaving what is typed alone.
    fn reload_stores(&mut self) {
        self.stores = memory::stores(self.workspace.as_deref());
        self.soul = memory::store(None, "soul");
        self.shown = self
            .stores
            .iter()
            .map(|store| {
                let entries = store.entries.as_deref().unwrap_or_default();
                entries
                    .iter()
                    .map(|entry| Shown {
                        text: memory::readable(entry),
                        flag: memory::flagged(entry),
                    })
                    .collect()
            })
            .collect();
        // The entry being edited may have moved while others were removed.
        if let Some((target, at, was)) = &mut self.editing
            && let Some(store) = self.stores.iter().find(|store| store.target == *target)
        {
            *at = relocated(store.entries.as_deref().unwrap_or_default(), *at, was);
        }
    }

    /// Read everything again, and SOUL.md into its box unless what is there
    /// has not been saved.
    fn reload(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.reload_stores();
        let typed = self.soul_text.read(cx).value().to_string();
        if !unsaved(&typed, &self.soul_loaded) {
            self.load_soul(window, cx);
        }
    }

    /// Put what SOUL.md holds now in its box, over anything typed there.
    fn load_soul(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let text = self
            .soul
            .as_ref()
            .and_then(|soul| soul.entries.as_ref().ok()?.first().cloned())
            .unwrap_or_default();
        self.soul_loaded = text.clone();
        self.soul_dirty = false;
        self.soul_text
            .update(cx, |input, cx| input.set_value(text, window, cx));
    }

    /// What a store is called, as USER.md.
    fn name(&self, target: &'static str) -> &'static str {
        self.stores
            .iter()
            .find(|store| store.target == target)
            .map_or(target, |store| store.name)
    }

    /// Whether an entry edit or SOUL.md holds typing not saved yet.
    fn has_unsaved(&self, cx: &App) -> bool {
        let entry = self
            .editing
            .as_ref()
            .is_some_and(|(_, _, was)| unsaved(&self.entry.read(cx).value(), was));
        entry || unsaved(&self.soul_text.read(cx).value(), &self.soul_loaded)
    }

    fn start_edit(
        &mut self,
        target: &'static str,
        at: usize,
        was: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some((open, number, before)) = &self.editing
            && (*open, *number) != (target, at)
            && unsaved(&self.entry.read(cx).value(), before)
        {
            self.notice = Some((
                format!(
                    "Entry {} of {} has changes not saved yet: save or cancel them first.",
                    number + 1,
                    self.name(open)
                ),
                true,
            ));
            cx.notify();
            return;
        }
        self.entry.update(cx, |input, cx| {
            input.set_value(was.clone(), window, cx);
            input.focus(window, cx);
        });
        self.editing = Some((target, at, was));
        cx.notify();
    }

    /// Save the entry being edited, or delete the one at `at` when `becomes`
    /// is None, and read the stores again.
    fn change(
        &mut self,
        target: &'static str,
        at: usize,
        was: String,
        becomes: Option<String>,
        cx: &mut Context<Self>,
    ) {
        let workspace = self.workspace.clone();
        // Off the UI thread: the store's lock can wait a second on a writer.
        let done = cx.background_executor().spawn(async move {
            memory::edit(workspace.as_deref(), target, at, &was, becomes.as_deref())
        });
        cx.spawn(async move |this, cx| {
            let done = done.await;
            let _ = this.update(cx, |this, cx| {
                let refused = done.is_err();
                this.notice = match done {
                    Ok(backup) => backup.map(|line| (line, false)),
                    Err(error) => Some((error, true)),
                };
                // A refused edit stays open, to be put right, and so does an
                // edit of another entry.
                let this_one = this
                    .editing
                    .as_ref()
                    .is_some_and(|(open, number, _)| (*open, *number) == (target, at));
                if !refused && this_one {
                    this.editing = None;
                }
                this.reload_stores();
                cx.notify();
            });
        })
        .detach();
    }

    fn save_edit(&mut self, cx: &mut Context<Self>) {
        let Some((target, at, was)) = self.editing.clone() else {
            return;
        };
        let text = self.entry.read(cx).value().to_string();
        self.change(target, at, was, Some(text), cx);
    }

    /// Save the box that has focus, for Cmd-S or Ctrl-S on the page.
    fn save_focused(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.editing.is_some() && self.entry.focus_handle(cx).is_focused(window) {
            self.save_edit(cx);
        } else if self.soul_text.focus_handle(cx).is_focused(window) {
            self.save_soul(window, cx);
        }
    }

    /// Save SOUL.md's box, asking first when that would empty the file.
    fn save_soul(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let text = self.soul_text.read(cx).value().to_string();
        if text.trim().is_empty() && !self.soul_loaded.trim().is_empty() {
            let page = cx.entity();
            super::confirm_delete(
                "Empty SOUL.md",
                "SOUL.md will be emptied, and Letronna's own first line used in its place. A copy of what it holds is kept beside it.".into(),
                window,
                cx,
                move |window, cx| {
                    page.update(cx, |this, cx| this.write_soul(String::new(), window, cx));
                },
            );
            return;
        }
        self.write_soul(text, window, cx);
    }

    /// Write SOUL.md, unless it changed since it was put in the box, then
    /// put what it holds back in the box.
    fn write_soul(&mut self, text: String, window: &mut Window, cx: &mut Context<Self>) {
        let was = self.soul_loaded.clone();
        let done = cx.background_executor().spawn(async move {
            memory::save_soul(&was, &text).map(|backup| (backup, memory::flagged(&text)))
        });
        cx.spawn_in(window, async move |this, cx| {
            let done = done.await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.reload_stores();
                this.notice = Some(match done {
                    Ok((backup, flag)) => {
                        this.load_soul(window, cx);
                        let backup = backup.map(|line| format!(" {line}")).unwrap_or_default();
                        match flag {
                            Some(found) => (
                                format!("SOUL.md saved.{backup} It holds {found}, and goes into every request as it is."),
                                true,
                            ),
                            None => (format!("SOUL.md saved.{backup}"), false),
                        }
                    }
                    Err(error) => (error, true),
                });
                cx.notify();
            });
        })
        .detach();
    }

    fn ask_delete(
        &mut self,
        target: &'static str,
        at: usize,
        was: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let page = cx.entity();
        let name = self.name(target);
        super::confirm_delete(
            "Delete entry",
            format!(
                "Entry {} of {name} will be removed: \"{}\"",
                at + 1,
                preview(&memory::readable(&was))
            ),
            window,
            cx,
            move |_, cx| {
                page.update(cx, |this, cx| {
                    this.change(target, at, was.clone(), None, cx)
                });
            },
        );
    }

    /// Open a store's file with the system, or the folder it will be in
    /// before anything is kept.
    fn open_file(&mut self, target: &'static str, cx: &mut Context<Self>) {
        match memory::file(self.workspace.as_deref(), target) {
            Ok(path) if path.is_file() => cx.open_with_system(&path),
            Ok(path) => {
                if let Some(folder) = path.parent() {
                    cx.open_with_system(folder);
                }
            }
            Err(error) => {
                self.notice = Some((error, true));
                cx.notify();
            }
        }
    }
}

/// How much of a deleted entry the question quotes, in characters: about a
/// line of the dialog.
const PREVIEW: usize = 80;

/// The start of an entry, as the question to delete it quotes it.
fn preview(entry: &str) -> String {
    let mut preview: String = entry.chars().take(PREVIEW).collect();
    if preview.len() < entry.len() {
        preview.push('…');
    }
    preview
}

/// Whether what is in a box differs from the text it started from, beyond
/// the spaces a save trims.
fn unsaved(typed: &str, was: &str) -> bool {
    typed.trim() != was.trim()
}

/// Where the entry `was`, edited as number `at`, is now: there still, moved
/// by a removal before it, or left at `at`, where saving it is refused.
fn relocated(entries: &[String], at: usize, was: &str) -> usize {
    if entries.get(at).is_some_and(|entry| entry == was) {
        return at;
    }
    entries.iter().position(|entry| entry == was).unwrap_or(at)
}

impl Render for MemoryPage {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let notice = self.notice.as_ref().map(|(text, refused)| {
            div()
                .text_sm()
                .text_color(if *refused {
                    cx.theme().danger
                } else {
                    cx.theme().muted_foreground
                })
                .child(text.clone())
        });
        let stores: Vec<Div> = self
            .stores
            .iter()
            .zip(&self.shown)
            .map(|(store, shown)| self.store_section(store, shown, cx))
            .collect();
        v_flex()
            .key_context("MemoryPage")
            .on_action(cx.listener(|this, _: &Save, window, cx| this.save_focused(window, cx)))
            // Escape that the box did not take would close settings; with
            // typing not saved, it says so instead.
            .on_action(cx.listener(|this, _: &crate::app::Dismiss, _, cx| {
                if this.has_unsaved(cx) {
                    this.notice = Some((
                        "Changes not saved yet: save or cancel them first.".into(),
                        true,
                    ));
                    cx.notify();
                } else {
                    cx.propagate();
                }
            }))
            .gap_5()
            .child(super::settings::heading(
                "Memory",
                "What Letronna keeps from one session to the next, as plain files you can open. All of it goes into every request, so the model keeps it short with its memory tool.",
                cx,
            ))
            .children(notice)
            .children(stores)
            .when(self.workspace.is_none(), |this| {
                this.child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child("Project memory shows here when settings are opened over a session with a project folder."),
                )
            })
            .children(self.soul_section(cx))
    }
}

impl MemoryPage {
    /// A store's name, what it is for, its budget and a way to its file.
    fn header(&self, store: &memory::Store, blurb: &'static str, cx: &Context<Self>) -> Div {
        let target = store.target;
        let over = store.used > store.limit;
        let share = store.used as f32 / store.limit.max(1) as f32 * 100.;
        let meter = Progress::new().value(share).h(px(4.));
        v_flex()
            .gap_2()
            .child(
                h_flex()
                    .items_start()
                    .justify_between()
                    .gap_3()
                    .child(
                        v_flex()
                            .min_w_0()
                            .gap_0p5()
                            .child(
                                div()
                                    .text_sm()
                                    .font_weight(FontWeight::MEDIUM)
                                    .child(store.name),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(blurb),
                            ),
                    )
                    .child(
                        h_flex()
                            .flex_shrink_0()
                            .gap_2()
                            .items_center()
                            .child(
                                div()
                                    .text_xs()
                                    .font_family(MONO_FONT)
                                    .text_color(if over {
                                        cx.theme().danger
                                    } else {
                                        cx.theme().muted_foreground
                                    })
                                    .child(store.budget()),
                            )
                            .child(
                                Button::new(SharedString::from(format!("open-{target}")))
                                    .framed(cx)
                                    .xsmall()
                                    .icon(Icon::new(AppIcon::Folder))
                                    .label("Open the file")
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.open_file(target, cx)
                                    })),
                            ),
                    ),
            )
            .child(if over {
                meter.bg(cx.theme().danger)
            } else {
                meter
            })
    }

    fn store_section(&self, store: &memory::Store, shown: &[Shown], cx: &Context<Self>) -> Div {
        let blurb = match store.target {
            "user" => "What Letronna knows about you.",
            "memory" => "Its own notes.",
            _ => {
                "Its notes on this session's project, kept in the data folder, never in the repository."
            }
        };
        let muted = cx.theme().muted_foreground;
        let body = match &store.entries {
            Err(error) => div()
                .text_sm()
                .text_color(cx.theme().danger)
                .child(error.clone()),
            Ok(entries) => {
                // An entry being edited that removals left past the end
                // keeps its box, and saving it is refused by the pin.
                let past_end = self
                    .editing
                    .as_ref()
                    .filter(|(open, at, _)| *open == store.target && *at >= entries.len())
                    .map(|(_, at, _)| self.number(*at, cx).child(self.editor(cx)));
                if entries.is_empty() && past_end.is_none() {
                    div().text_sm().text_color(muted).child("Nothing kept yet.")
                } else {
                    div()
                        .children(entries.iter().zip(shown).enumerate().map(
                            |(at, (entry, shown))| {
                                self.entry_row(store.target, at, entry, shown, cx)
                            },
                        ))
                        .children(past_end)
                }
            }
        };
        v_flex()
            .gap_2()
            .pb_4()
            .border_b_1()
            .border_color(cx.theme().border)
            .child(self.header(store, blurb, cx))
            .child(body)
    }

    /// A row led by an entry's number.
    fn number(&self, at: usize, cx: &Context<Self>) -> Div {
        h_flex().items_start().gap_3().py_1p5().child(
            div()
                .w(px(20.))
                .flex_shrink_0()
                .pt_0p5()
                .text_xs()
                .font_family(MONO_FONT)
                .text_color(cx.theme().muted_foreground)
                .child((at + 1).to_string()),
        )
    }

    /// The box the open entry is edited in, with Save and Cancel.
    fn editor(&self, cx: &Context<Self>) -> Div {
        v_flex()
            .flex_1()
            .min_w_0()
            .gap_2()
            .child(Input::new(&self.entry).small().w_full())
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        Button::new("save-entry")
                            .primary()
                            .small()
                            .label("Save")
                            .on_click(cx.listener(|this, _, _, cx| this.save_edit(cx))),
                    )
                    .child(
                        Button::new("cancel-entry")
                            .plain(cx)
                            .small()
                            .label("Cancel")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.editing = None;
                                this.notice = None;
                                cx.notify();
                            })),
                    ),
            )
    }

    /// One entry under its number with Edit and Delete, or the box it is
    /// being edited in.
    fn entry_row(
        &self,
        target: &'static str,
        at: usize,
        entry: &str,
        shown: &Shown,
        cx: &Context<Self>,
    ) -> Div {
        let row = self.number(at, cx);
        let editing = self
            .editing
            .as_ref()
            .is_some_and(|(store, number, _)| *store == target && *number == at);
        if editing {
            return row.child(self.editor(cx));
        }
        let flag = shown.flag.as_ref().map(|found| {
            div().text_xs().text_color(cx.theme().danger).child(format!(
                "Holds {found}; the model is told to act on none of it."
            ))
        });
        let (edit_was, delete_was) = (entry.to_owned(), entry.to_owned());
        row.child(
            v_flex()
                .flex_1()
                .min_w_0()
                .gap_0p5()
                .child(div().text_sm().child(shown.text.clone()))
                .children(flag),
        )
        .child(
            h_flex()
                .flex_shrink_0()
                .gap_1()
                .child(
                    Button::new(SharedString::from(format!("edit-{target}-{at}")))
                        .plain(cx)
                        .xsmall()
                        .icon(Icon::new(AppIcon::Pencil))
                        .tooltip("Edit")
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.start_edit(target, at, edit_was.clone(), window, cx)
                        })),
                )
                .child(
                    Button::new(SharedString::from(format!("delete-{target}-{at}")))
                        .plain(cx)
                        .xsmall()
                        .icon(Icon::new(AppIcon::Trash))
                        .tooltip("Delete")
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.ask_delete(target, at, delete_was.clone(), window, cx)
                        })),
                ),
        )
    }

    /// SOUL.md in an editor of its own, as it is one piece of writing rather
    /// than entries.
    fn soul_section(&self, cx: &Context<Self>) -> Option<Div> {
        let soul = self.soul.as_ref()?;
        let changed = unsaved(&self.soul_text.read(cx).value(), &self.soul_loaded);
        let error = soul.entries.as_ref().err().map(|error| {
            div()
                .text_sm()
                .text_color(cx.theme().danger)
                .child(error.clone())
        });
        Some(
            v_flex()
                .gap_2()
                .child(self.header(
                    soul,
                    "Who Letronna is. Once written, it takes the place of Letronna's own first line in every request.",
                    cx,
                ))
                .children(error)
                .child(Input::new(&self.soul_text).w_full())
                .child(
                    h_flex()
                        .gap_2()
                        .items_center()
                        .child(
                            Button::new("save-soul")
                                .primary()
                                .small()
                                .label("Save")
                                .on_click(
                                    cx.listener(|this, _, window, cx| this.save_soul(window, cx)),
                                ),
                        )
                        .when(changed, |this| {
                            this.child(
                                Button::new("cancel-soul")
                                    .plain(cx)
                                    .small()
                                    .label("Cancel")
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        // What the file holds now, which may
                                        // be newer than what was typed over.
                                        this.reload_stores();
                                        this.load_soul(window, cx);
                                        this.notice = None;
                                        cx.notify();
                                    })),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(cx.theme().muted_foreground)
                                    .child("Not saved"),
                            )
                        }),
                ),
        )
    }
}

#[cfg(test)]
mod tests {
    // Not `use super::*`: the prelude re-exports gpui, whose `test` macro
    // would shadow the built-in attribute.
    use super::{PREVIEW, preview, relocated, unsaved};

    #[test]
    fn a_long_entry_is_quoted_cut_short_between_characters() {
        assert_eq!(preview("Uses zsh."), "Uses zsh.");
        let long = "é".repeat(PREVIEW + 5);
        let quoted = preview(&long);
        assert_eq!(quoted.chars().count(), PREVIEW + 1);
        assert!(quoted.ends_with("é…"));
        assert_eq!(preview(&"x".repeat(PREVIEW)), "x".repeat(PREVIEW));
    }

    #[test]
    fn typing_is_unsaved_only_when_it_differs_beyond_spaces() {
        assert!(!unsaved("Be brief.\n", " Be brief."));
        assert!(!unsaved("", ""));
        assert!(unsaved("Be brief. Be kind.", "Be brief."));
        assert!(unsaved("", "Be brief."), "emptying the box is a change");
    }

    #[test]
    fn an_entry_being_edited_follows_removals_before_it() {
        let entries: Vec<String> = ["Uses zsh.", "Works late."].map(String::from).into();
        assert_eq!(relocated(&entries, 1, "Works late."), 1);
        // Entry 1 of three was removed, so the third is now the second.
        assert_eq!(relocated(&entries, 2, "Works late."), 1);
        // Changed by another writer: left where it was, for the pin to refuse.
        assert_eq!(relocated(&entries, 1, "Works early."), 1);
        assert_eq!(relocated(&[], 3, "Gone."), 3);
    }
}
