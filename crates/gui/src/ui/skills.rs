//! The Skills page: saved prompts, loaded into the composer on demand.

use crate::prelude::*;
use letronna_core::skills::{self, Skill};

impl Chat {
    fn save_skills(&mut self, cx: &mut Context<Self>) {
        skills::save(&self.skills);
        cx.notify();
    }

    fn new_skill(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let skill = Skill {
            id: skills::next_id(&self.skills),
            name: String::new(),
            prompt: String::new(),
        };
        self.editing_skill = Some(skill.id);
        self.skills.push(skill);
        for input in [&self.skill_name, &self.skill_prompt] {
            input.update(cx, |input, cx| input.set_value("", window, cx));
        }
        self.save_skills(cx);
    }

    /// Put the skill's prompt in the composer of the active chat, ready to
    /// adjust. Sending it is left to the user.
    fn use_skill(&mut self, id: u64, window: &mut Window, cx: &mut Context<Self>) {
        let Some(skill) = self.skills.iter().find(|s| s.id == id) else {
            return;
        };
        let prompt = skill.prompt.clone();
        self.page = Page::Chat;
        self.composer
            .update(cx, |input, cx| input.set_value(prompt, window, cx));
        self.composer
            .update(cx, |input, cx| input.focus(window, cx));
        cx.notify();
    }

    fn delete_skill(&mut self, id: u64, window: &mut Window, cx: &mut Context<Self>) {
        let Some(skill) = self.skills.iter().find(|s| s.id == id) else {
            return;
        };
        let body = format!("\"{}\" will be removed from this device.", skill.name);
        let chat = cx.entity();
        crate::ui::confirm_delete("Delete skill", body, window, cx, move |_, cx| {
            chat.update(cx, |this, cx| {
                this.skills.retain(|s| s.id != id);
                this.save_skills(cx);
            })
        });
    }

    fn edit_skill(&mut self, id: u64, window: &mut Window, cx: &mut Context<Self>) {
        let Some(skill) = self.skills.iter().find(|s| s.id == id) else {
            return;
        };
        let (name, prompt) = (skill.name.clone(), skill.prompt.clone());
        self.skill_name
            .update(cx, |input, cx| input.set_value(name, window, cx));
        self.skill_prompt
            .update(cx, |input, cx| input.set_value(prompt, window, cx));
        self.editing_skill = Some(id);
        cx.notify();
    }

    fn commit_skill(&mut self, id: u64, cx: &mut Context<Self>) {
        let name = self.skill_name.read(cx).value().trim().to_owned();
        let prompt = self.skill_prompt.read(cx).value().trim().to_owned();
        if prompt.is_empty() {
            self.error = Some("A skill needs a prompt.".into());
            cx.notify();
            return;
        }
        if let Some(skill) = self.skills.iter_mut().find(|s| s.id == id) {
            skill.name = if name.is_empty() {
                prompt.chars().take(40).collect()
            } else {
                name
            };
            skill.prompt = prompt;
        }
        self.editing_skill = None;
        self.save_skills(cx);
    }

    fn skill_editor(&self, id: u64, cx: &mut Context<Self>) -> impl IntoElement {
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
                            .on_click(cx.listener(move |this, _, _, cx| this.commit_skill(id, cx))),
                    )
                    .child(
                        Button::new("cancel-skill")
                            .plain(cx)
                            .small()
                            .label("Cancel")
                            .on_click(cx.listener(move |this, _, _, cx| {
                                // Never given a prompt means never really made.
                                this.skills.retain(|s| s.id != id || !s.prompt.is_empty());
                                this.editing_skill = None;
                                this.save_skills(cx);
                            })),
                    ),
            )
    }

    fn skill_row(&self, index: usize, cx: &mut Context<Self>) -> impl IntoElement {
        let skill = &self.skills[index];
        let id = skill.id;
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
                            .child(letronna_core::tools::one_line(&skill.prompt)),
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
                                this.use_skill(id, window, cx)
                            })),
                    )
                    .child(
                        Button::new(("edit-skill", index))
                            .plain(cx)
                            .xsmall()
                            .icon(Icon::new(AppIcon::Pencil))
                            .tooltip("Edit")
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.edit_skill(id, window, cx)
                            })),
                    )
                    .child(
                        Button::new(("delete-skill", index))
                            .plain(cx)
                            .xsmall()
                            .icon(Icon::new(AppIcon::Trash))
                            .tooltip("Delete")
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.delete_skill(id, window, cx)
                            })),
                    ),
            )
    }

    pub(crate) fn skills_page(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let rows: Vec<AnyElement> = (0..self.skills.len())
            .map(|index| {
                if self.editing_skill == Some(self.skills[index].id) {
                    self.skill_editor(self.skills[index].id, cx)
                        .into_any_element()
                } else {
                    self.skill_row(index, cx).into_any_element()
                }
            })
            .collect();
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
                        crate::ui::page_header(
                            "Skills",
                            "Prompts you reach for often. Using one puts it in the composer so \
                             you can adjust it before sending.",
                            cx,
                        )
                        .flex_1()
                        .min_w_0(),
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
