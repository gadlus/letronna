use crate::prelude::*;
use gpui_component::menu::PopupMenu;

impl Chat {
    pub(crate) fn session_menu(
        &self,
        index: usize,
        cx: &mut Context<Self>,
    ) -> impl Fn(PopupMenu, &mut Window, &mut Context<PopupMenu>) -> PopupMenu + 'static {
        let chat = cx.entity();
        let pinned = self.store.conversations[index].pinned;
        move |menu, _, _| {
            let item =
                |label: &'static str,
                 icon: AppIcon,
                 f: fn(&mut Chat, usize, &mut Window, &mut Context<Chat>)| {
                    let chat = chat.clone();
                    PopupMenuItem::new(label).icon(Icon::new(icon)).on_click(
                        move |_, window, cx| chat.update(cx, |this, cx| f(this, index, window, cx)),
                    )
                };
            menu.min_w(px(200.))
                .item(item("Rename", AppIcon::Pencil, Chat::rename_session))
                .item(item(
                    if pinned { "Unpin" } else { "Pin" },
                    AppIcon::Pin,
                    Chat::toggle_pin,
                ))
                .separator()
                .item(item(
                    "Export as Markdown",
                    AppIcon::Export,
                    Chat::export_session,
                ))
                .separator()
                .item(item("Delete", AppIcon::Trash, Chat::delete_session))
        }
    }

    pub(crate) fn toggle_pin(&mut self, index: usize, _: &mut Window, cx: &mut Context<Self>) {
        self.store.conversations[index].pinned ^= true;
        self.save();
        cx.notify();
    }

    pub(crate) fn rename_session(
        &mut self,
        index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let title = self.store.conversations[index].title.clone();
        self.rename
            .update(cx, |input, cx| input.set_value(title, window, cx));
        let input = self.rename.clone();
        let chat = cx.entity();
        window.open_dialog(cx, move |dialog, _, _| {
            let input = input.clone();
            let chat = chat.clone();
            dialog
                .title(div().font_family(HEADING_FONT).child("Rename session"))
                .w(px(420.))
                .confirm()
                .child(Input::new(&input))
                .on_ok(move |_, _, cx| {
                    let title = input.read(cx).value().trim().to_string();
                    chat.update(cx, |this, cx| {
                        if !title.is_empty() {
                            this.store.conversations[index].title = title;
                            this.save();
                            cx.notify();
                        }
                    });
                    true
                })
        });
        // After opening, which takes focus for the dialog itself; the name
        // is what is being edited, so the cursor belongs there.
        self.rename.update(cx, |input, cx| input.focus(window, cx));
    }

    pub(crate) fn delete_session(
        &mut self,
        index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let title = self.store.conversations[index].title.clone();
        let chat = cx.entity();
        super::confirm_delete(
            "Delete session",
            format!("\"{title}\" and its messages will be removed from this device."),
            window,
            cx,
            move |window, cx| {
                chat.update(cx, |this, cx| {
                    if this.store.conversations.len() <= index {
                        return;
                    }
                    // Said out loud: the dialog closes either way, and closing
                    // it on nothing looked like the delete had been lost.
                    if this.busy {
                        this.error = Some("Stop the reply before deleting a session.".into());
                        cx.notify();
                        return;
                    }
                    // Through the store, so the session's file goes too,
                    // and its private browser with it.
                    letronna_core::browser::forget(this.store.conversations[index].id);
                    // The next new session can be given this id again, and
                    // must not start out with what was allowed here.
                    this.session_allow
                        .remove(&this.store.conversations[index].id);
                    this.store.remove(index);
                    if this.store.conversations.is_empty() {
                        this.new_chat(window, cx);
                        return;
                    }
                    if this.store.active >= index && this.store.active > 0 {
                        this.store.active -= 1;
                    }
                    this.save();
                    cx.notify();
                });
            },
        );
    }

    pub(crate) fn export_session(
        &mut self,
        index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let c = &self.store.conversations[index];
        let mut text = format!("# {}\n\n", c.title);
        for m in &c.messages {
            let who = if m.notice {
                "Notice".to_string()
            } else if m.role == "user" {
                "You".to_string()
            } else if m.model.is_empty() {
                "Assistant".to_string()
            } else {
                m.model.clone()
            };
            text.push_str(&format!("**{who}**\n\n{}\n\n---\n\n", m.content));
        }
        let name: String = c
            .title
            .chars()
            .map(|ch| {
                if ch.is_alphanumeric() || ch == ' ' {
                    ch
                } else {
                    '-'
                }
            })
            .collect::<String>()
            .trim()
            .to_string();
        let home = std::env::var_os("USERPROFILE")
            .or_else(|| std::env::var_os("HOME"))
            .map(PathBuf::from)
            .unwrap_or_default();
        let receiver = cx.prompt_for_new_path(&home, Some(&format!("{name}.md")));
        cx.spawn_in(window, async move |this, cx| {
            if let Ok(Ok(Some(path))) = receiver.await {
                let failed = std::fs::write(&path, text).is_err();
                let _ = this.update(cx, |this, cx| {
                    if failed {
                        this.error = Some("Could not write the export file.".into());
                    }
                    cx.notify();
                });
            }
        })
        .detach();
    }
}
