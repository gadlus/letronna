//! The Skills page: SKILL.md folders, added from GitHub, a link or a folder
//! and switched on and off. The agent loads one that is on when a task calls
//! for it, and Use puts its instructions in the composer.

use crate::prelude::*;
use letronna_core::skills::{self, Source};

impl Chat {
    /// Read the skills again, as the active session's project sees them.
    pub(crate) fn reload_skills(&mut self) {
        let workspace = self.store.conversations[self.store.active]
            .workspace
            .clone();
        (self.skills, self.skills_error) = match skills::list(workspace.as_deref()) {
            Ok(list) => (list, None),
            Err(why) => (Vec::new(), Some(why)),
        };
    }

    /// Add the skills the box names. The download runs in the background,
    /// and a failure leaves the box as it was so it can be corrected.
    pub(crate) fn add_skills(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let source = self.skill_source.read(cx).value().trim().to_owned();
        if source.is_empty() || self.adding_skills {
            return;
        }
        self.adding_skills = true;
        self.skills_note = None;
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let added = cx
                .background_executor()
                .spawn(async move { skills::install(&source) })
                .await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.adding_skills = false;
                if added.is_ok() {
                    this.skill_source
                        .update(cx, |input, cx| input.set_value("", window, cx));
                }
                this.skills_note = Some(added.map(|names| match names.as_slice() {
                    [one] => format!("Added {one}"),
                    names => format!("Added {} skills", names.len()),
                }));
                this.reload_skills();
                cx.notify();
            });
        })
        .detach();
    }

    /// Add the skills in a folder picked on this computer. Windows' picker
    /// shows folders or files, never both, so there it is folders; a single
    /// file's path can be typed into the box.
    fn choose_skills(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let receiver = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: true,
            multiple: false,
            prompt: None,
        });
        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(paths))) = receiver.await else {
                return;
            };
            let Some(path) = paths.into_iter().next() else {
                return;
            };
            let _ = this.update_in(cx, |this, window, cx| {
                let path = path.display().to_string();
                this.skill_source
                    .update(cx, |input, cx| input.set_value(path, window, cx));
                this.add_skills(window, cx);
            });
        })
        .detach();
    }

    /// Switch a skill on or off, for every session that can see it.
    fn switch_skill(&mut self, name: &str, on: bool, cx: &mut Context<Self>) {
        match skills::set_enabled(name, on) {
            Ok(()) => {
                if let Some(skill) = self.skills.iter_mut().find(|s| s.name == name) {
                    skill.enabled = on;
                }
            }
            Err(why) => self.error = Some(why),
        }
        cx.notify();
    }

    fn new_skill(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.editing_skill = Some(String::new());
        for input in [&self.skill_name, &self.skill_prompt] {
            input.update(cx, |input, cx| input.set_value("", window, cx));
        }
        cx.notify();
    }

    /// Put the skill's instructions in the composer of the active chat, ready
    /// to adjust. Sending them is left to the user.
    fn use_skill(&mut self, name: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(skill) = skills::find(&self.skills, name) else {
            return;
        };
        let prompt = skill.body.clone();
        self.page = Page::Chat;
        self.composer
            .update(cx, |input, cx| input.set_value(prompt, window, cx));
        self.composer
            .update(cx, |input, cx| input.focus(window, cx));
        cx.notify();
    }

    fn delete_skill(&mut self, name: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(skill) = skills::find(&self.skills, name) else {
            return;
        };
        let name = skill.name.clone();
        let body = format!("\"{name}\" will be moved to the skills archive on this device.");
        let chat = cx.entity();
        crate::ui::confirm_delete("Delete skill", body, window, cx, move |_, cx| {
            chat.update(cx, |this, cx| {
                this.skills_note = Some(
                    skills::remove(&name)
                        .map(|place| format!("Moved {name} to {}", place.display())),
                );
                this.reload_skills();
                cx.notify();
            })
        });
    }

    fn edit_skill(&mut self, name: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(skill) = skills::find(&self.skills, name) else {
            return;
        };
        let (name, prompt) = (skill.name.clone(), skill.body.clone());
        self.skill_name
            .update(cx, |input, cx| input.set_value(name.clone(), window, cx));
        self.skill_prompt
            .update(cx, |input, cx| input.set_value(prompt, window, cx));
        self.editing_skill = Some(name);
        cx.notify();
    }

    fn commit_skill(&mut self, cx: &mut Context<Self>) {
        let Some(editing) = self.editing_skill.clone() else {
            return;
        };
        let name = self.skill_name.read(cx).value().trim().to_owned();
        let prompt = self.skill_prompt.read(cx).value().trim().to_owned();
        if prompt.is_empty() {
            self.error = Some("A skill needs instructions.".into());
            cx.notify();
            return;
        }
        let name = if name.is_empty() {
            prompt.chars().take(40).collect()
        } else {
            name
        };
        let saved = if editing.is_empty() {
            skills::create(&name, "", &prompt)
        } else {
            skills::edit(&editing, &name, None, &prompt)
        };
        match saved {
            Ok(_) => self.editing_skill = None,
            Err(why) => self.error = Some(why),
        }
        self.reload_skills();
        cx.notify();
    }

    fn skill_editor(&self, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .w_full()
            .gap_3()
            .p_3()
            .rounded(cx.theme().radius)
            .border_1()
            .border_color(cx.theme().primary.opacity(0.4))
            .bg(cx.theme().secondary.opacity(0.3))
            .child(
                v_flex()
                    .gap_1p5()
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child("Name"),
                    )
                    .child(Input::new(&self.skill_name).small().w_full()),
            )
            .child(
                v_flex()
                    .gap_1p5()
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child("Instructions"),
                    )
                    .child(Input::new(&self.skill_prompt).small().w_full()),
            )
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        Button::new("save-skill")
                            .primary()
                            .small()
                            .label("Save")
                            .on_click(cx.listener(move |this, _, _, cx| this.commit_skill(cx))),
                    )
                    .child(
                        Button::new("cancel-skill")
                            .ghost()
                            .small()
                            .label("Cancel")
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.editing_skill = None;
                                cx.notify();
                            })),
                    ),
            )
    }

    fn skill_row(&self, index: usize, cx: &mut Context<Self>) -> impl IntoElement {
        let skill = &self.skills[index];
        let (name, enabled) = (skill.name.clone(), skill.enabled);
        // A project's skills are changed in the project's folder.
        let yours = skill.source == Source::User;
        let (edit_name, delete_name, switch_name) = (name.clone(), name.clone(), name.clone());
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
                                    .min_w_0()
                                    .text_sm()
                                    .font_weight(FontWeight::MEDIUM)
                                    .truncate()
                                    .when(!enabled, |this| {
                                        this.text_color(cx.theme().muted_foreground)
                                    })
                                    .child(skill.name.clone()),
                            )
                            .when(!yours, |this| {
                                this.child(
                                    div()
                                        .flex_shrink_0()
                                        .px_1p5()
                                        .rounded(cx.theme().radius)
                                        .border_1()
                                        .border_color(cx.theme().border)
                                        .text_xs()
                                        .text_color(cx.theme().muted_foreground)
                                        .child("Project"),
                                )
                            }),
                    )
                    .child(
                        // Two lines at most, cut between words: gpui places
                        // its ellipsis before it knows the row's width, and
                        // here that cut letters in half.
                        div()
                            .max_h(px(36.))
                            .overflow_hidden()
                            .text_xs()
                            .line_height(px(18.))
                            .text_color(cx.theme().muted_foreground)
                            .child(skill.description.clone()),
                    ),
            )
            .child(
                h_flex()
                    .flex_shrink_0()
                    .gap_1()
                    .items_center()
                    .child(
                        Button::new(("use", index))
                            .ghost()
                            .xsmall()
                            .label("Use")
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.use_skill(&name, window, cx)
                            })),
                    )
                    .when(yours, |this| {
                        this.child(
                            Button::new(("edit-skill", index))
                                .ghost()
                                .xsmall()
                                .icon(Icon::new(AppIcon::Pencil))
                                .tooltip("Edit")
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    this.edit_skill(&edit_name, window, cx)
                                })),
                        )
                        .child(
                            Button::new(("delete-skill", index))
                                .ghost()
                                .xsmall()
                                .icon(Icon::new(AppIcon::Trash))
                                .tooltip("Delete")
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    this.delete_skill(&delete_name, window, cx)
                                })),
                        )
                    })
                    .child(
                        div().ml_1().cursor_pointer().child(
                            gpui_component::switch::Switch::new(("skill-on", index))
                                .checked(enabled)
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.switch_skill(&switch_name, !enabled, cx)
                                })),
                        ),
                    ),
            )
    }

    /// The box and buttons that add skills from elsewhere.
    fn skill_adder(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let adding = self.adding_skills;
        h_flex()
            .gap_2()
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .child(Input::new(&self.skill_source).small().disabled(adding)),
            )
            .child(
                Button::new("install-skills")
                    .primary()
                    .small()
                    .label(if adding { "Adding…" } else { "Add" })
                    .loading(adding)
                    .disabled(adding)
                    .on_click(cx.listener(|this, _, window, cx| this.add_skills(window, cx))),
            )
            .child(
                Button::new("choose-skills")
                    .ghost()
                    .small()
                    .icon(Icon::new(AppIcon::Folder))
                    .label("Choose folder…")
                    .disabled(adding)
                    .on_click(cx.listener(|this, _, window, cx| this.choose_skills(window, cx))),
            )
    }

    pub(crate) fn skills_page(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let rows: Vec<AnyElement> = (0..self.skills.len())
            .map(|index| {
                if self.editing_skill.as_ref() == Some(&self.skills[index].name) {
                    self.skill_editor(cx).into_any_element()
                } else {
                    self.skill_row(index, cx).into_any_element()
                }
            })
            .collect();
        // A new skill is written only once it is saved, so its editor stands
        // above the list until then, in sight however far that is scrolled.
        let new_skill = self
            .editing_skill
            .as_ref()
            .is_some_and(String::is_empty)
            .then(|| self.skill_editor(cx).into_any_element());
        let note = self.skills_note.as_ref().map(|note| {
            let (text, color) = match note {
                Ok(text) => (text.clone(), cx.theme().muted_foreground),
                Err(why) => (why.clone(), cx.theme().danger),
            };
            div().text_sm().text_color(color).child(text)
        });
        let on = self.skills.iter().filter(|s| s.enabled).count();
        let summary = match &self.skills_error {
            Some(why) => div()
                .text_sm()
                .text_color(cx.theme().danger)
                .child(why.clone()),
            None if self.skills.is_empty() => div()
                .text_sm()
                .text_color(cx.theme().muted_foreground)
                .child(
                    "No skills yet. Add a GitHub repository of them, such as anthropics/skills, \
                     or write your own.",
                ),
            None => div()
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .child(format!("{on} of {} on", self.skills.len())),
        };
        v_flex()
            .flex_1()
            .min_h_0()
            .px_8()
            .py_6()
            .gap_4()
            .child(
                h_flex()
                    .items_center()
                    .justify_between()
                    .gap_4()
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .gap_1()
                            .child(
                                div()
                                    .text_xl()
                                    .font_family(HEADING_FONT)
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child("Skills"),
                            )
                            .child(
                                div()
                                    .text_sm()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(
                                        "Instructions for a kind of task. Letronna loads a skill \
                                         that is on when a task calls for it, and Use puts one \
                                         in the composer to adjust first.",
                                    ),
                            ),
                    )
                    .child(
                        Button::new("add-skill")
                            .primary()
                            .small()
                            .flex_shrink_0()
                            .icon(Icon::new(IconName::Plus))
                            .label("New skill")
                            .on_click(
                                cx.listener(|this, _, window, cx| this.new_skill(window, cx)),
                            ),
                    ),
            )
            .child(v_flex().gap_2().child(self.skill_adder(cx)).children(note))
            .children(new_skill)
            .child(summary)
            .child(
                // A column itself, so a row with little to say still
                // stretches the full width.
                v_flex()
                    .id("skill-list")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .gap_2()
                    .children(rows),
            )
    }
}
