//! The Skills page: skill folders, whose instructions load into the composer
//! on demand.

use crate::prelude::*;
use letronna_core::skills;

impl Chat {
    /// Read the skills again, as the active session's project sees them.
    pub(crate) fn reload_skills(&mut self) {
        let workspace = self.store.conversations[self.store.active]
            .workspace
            .clone();
        match skills::list(workspace.as_deref()) {
            Ok(list) => self.skills = list,
            Err(why) => self.error = Some(why),
        }
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
        self.composer.update(cx, |input, cx| {
            let typed = input.value().trim_end().to_owned();
            let value = if typed.is_empty() {
                prompt
            } else {
                format!("{typed}\n\n{prompt}")
            };
            // With the caret after it, where adjusting it starts.
            input.set_value("", window, cx);
            input.insert(value, window, cx);
        });
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
                if let Err(why) = skills::remove(&name) {
                    this.error = Some(why);
                }
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
            self.error = Some("A skill needs a prompt.".into());
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
            // Clear of the hairlines of the rows either side.
            .my_2()
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
                            .child("Prompt"),
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
                            .plain(cx)
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
        let name = skill.name.clone();
        let (edit_name, delete_name) = (name.clone(), name.clone());
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
                        div()
                            .text_sm()
                            .font_weight(FontWeight::MEDIUM)
                            .truncate()
                            .child(skill.name.clone()),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .truncate()
                            .child(skill.description.clone()),
                    ),
            )
            .child(
                h_flex()
                    .gap_1()
                    .items_center()
                    .child(
                        Button::new(("use", index))
                            .plain(cx)
                            .xsmall()
                            .label("Use")
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.use_skill(&name, window, cx)
                            })),
                    )
                    .child(
                        Button::new(("edit-skill", index))
                            .plain(cx)
                            .xsmall()
                            .icon(Icon::new(AppIcon::Pencil))
                            .tooltip("Edit")
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.edit_skill(&edit_name, window, cx)
                            })),
                    )
                    .child(
                        Button::new(("delete-skill", index))
                            .plain(cx)
                            .xsmall()
                            .icon(Icon::new(AppIcon::Trash))
                            .tooltip("Delete")
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.delete_skill(&delete_name, window, cx)
                            })),
                    ),
            )
    }

    pub(crate) fn skills_page(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let mut rows: Vec<AnyElement> = (0..self.skills.len())
            .map(|index| {
                if self.editing_skill.as_ref() == Some(&self.skills[index].name) {
                    self.skill_editor(cx).into_any_element()
                } else {
                    self.skill_row(index, cx).into_any_element()
                }
            })
            .collect();
        // A new skill is written only once it is saved, so its editor stands
        // at the top until then.
        if self.editing_skill.as_ref().is_some_and(String::is_empty) {
            rows.insert(0, self.skill_editor(cx).into_any_element());
        }
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
                                        "Prompts you reach for often. Using one puts it in the \
                                         composer so you can adjust it before sending.",
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
            .child(
                div()
                    .id("skill-list")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .child(v_flex().children(rows)),
            )
    }
}
