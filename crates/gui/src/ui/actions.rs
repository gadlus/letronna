//! What can be done with a turn once it is there: copy it, change your
//! message and ask again, ask the same again, go back to it, or carry on
//! from it in a copy of the session. Each turn offers these in a row of
//! small buttons and on right-click.
//!
//! Everything works by place in the session. Going back is saved as a
//! rewind record the store appends (`Conversation::rewind`), so the terminal
//! sees the shorter session too. It can also put back the files the agent's
//! tools wrote since, from the copies `history` kept of them.

use crate::prelude::*;
use gpui_component::checkbox::Checkbox;
use gpui_component::menu::PopupMenu;
use gpui_component::notification::Notification;
use gpui_component::{button::ButtonVariant, dialog::DialogButtonProps};
use letronna_core::history;
use std::{cell::Cell, rc::Rc};

/// A turn's place in its session: your message on its own, or everything
/// the model did in reply to it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Span {
    pub(crate) first: usize,
    pub(crate) last: usize,
    pub(crate) yours: bool,
}

impl Span {
    /// Where the prompt this turn is about sits: your message itself, or the
    /// one just before the reply.
    fn prompt(self) -> Option<usize> {
        if self.yours {
            Some(self.first)
        } else {
            self.first.checked_sub(1)
        }
    }

    /// How much of the session going back to this turn keeps: everything
    /// before your message, or everything up to the end of the reply.
    fn keep(self) -> usize {
        if self.yours {
            self.first
        } else {
            self.last + 1
        }
    }
}

/// An action on a turn, as the menu and the buttons both call it.
type Act = fn(&mut Chat, Span, &mut Window, &mut Context<Chat>);

impl Chat {
    fn open_messages(&self) -> &[Message] {
        &self.store.conversations[self.store.active].messages
    }

    /// The text of your message at `at`, if that is what is there.
    fn prompt_text(&self, at: usize) -> Option<String> {
        self.open_messages()
            .get(at)
            .filter(|m| m.role == "user")
            .map(|m| m.content.clone())
    }

    /// What a turn says, for copying: your message, or the reply's words
    /// without its tool calls.
    fn turn_text(&self, span: Span) -> String {
        let messages = self.open_messages();
        let last = span.last.min(messages.len().saturating_sub(1));
        messages[span.first.min(last)..=last]
            .iter()
            .filter(|m| m.role != "tool" && !m.content.is_empty())
            .map(|m| m.content.as_str())
            .collect::<Vec<_>>()
            .join("\n\n")
    }

    /// Cut the open session back to its first `keep` messages and save it.
    fn cut_to(&mut self, keep: usize) {
        let ix = self.store.active;
        let id = self.store.conversations[ix].id;
        let conversation = &mut self.store.conversations[ix];
        conversation.rewind(keep);
        // The checklist is the one the last `todo` left standing.
        conversation.todos = todos_in(&conversation.messages);
        self.thinking_open
            .retain(|&(session, at)| session != id || at < keep);
        self.save();
    }

    pub(crate) fn copy_turn(&mut self, span: Span, window: &mut Window, cx: &mut Context<Self>) {
        cx.write_to_clipboard(ClipboardItem::new_string(self.turn_text(span)));
        window.push_notification(Notification::success("Copied."), cx);
    }

    /// Change your message where it is. Nothing is taken back until it is
    /// sent, so looking and cancelling costs nothing.
    pub(crate) fn edit_turn(&mut self, span: Span, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        let Some(text) = self.prompt_text(span.first) else {
            return;
        };
        let session = self.store.conversations[self.store.active].id;
        self.editing_message = Some((session, span.first));
        self.edit_message.update(cx, |input, cx| {
            input.set_value(text, window, cx);
            input.focus(window, cx);
        });
        cx.notify();
    }

    pub(crate) fn cancel_edit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.editing_message = None;
        self.focus_composer(window, cx);
        cx.notify();
    }

    /// Send the changed message: the session goes back to just before it,
    /// and it is asked as if new.
    pub(crate) fn send_edit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some((session, at)) = self.editing_message else {
            return;
        };
        if self.busy {
            return;
        }
        let text = self.edit_message.read(cx).value().trim().to_owned();
        if text.is_empty() {
            return;
        }
        if text.chars().count() > 12_000 {
            self.error = Some("Please keep each message under 12,000 characters.".into());
            cx.notify();
            return;
        }
        self.editing_message = None;
        // Switched away since: the edit was for another session.
        if self.store.conversations[self.store.active].id != session {
            cx.notify();
            return;
        }
        self.cut_to(at);
        self.ask_again(text, window, cx);
    }

    /// Ask the turn's prompt again, dropping what came of it last time.
    pub(crate) fn retry_turn(&mut self, span: Span, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        let Some(at) = span.prompt() else {
            return;
        };
        let Some(text) = self.prompt_text(at) else {
            return;
        };
        self.cut_to(at);
        self.ask_again(text, window, cx);
    }

    fn ask_again(&mut self, text: String, window: &mut Window, cx: &mut Context<Self>) {
        if self.dispatch(text, None, cx).is_err() {
            self.open_settings(window, cx);
        }
    }

    /// Go back to this turn, once asked: what follows it leaves the session.
    /// From your message, its text comes back to the box, as in Claude Code.
    pub(crate) fn rewind_turn(&mut self, span: Span, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        let keep = span.keep();
        if self.open_messages().len() <= keep {
            return;
        }
        let back = span.yours.then(|| self.prompt_text(span.first)).flatten();
        let open = &self.store.conversations[self.store.active];
        let (session, folder) = (open.id, open.workspace.clone());
        let dropped = open.messages[keep..].to_vec();
        // Each file the dropped turns wrote is read and hashed, which a big
        // one makes slow, so the question waits for it off the UI thread.
        let planned = cx.background_executor().spawn(async move {
            let files = history::plan(session, &dropped, folder.as_deref());
            let mut rows = history::listing(&files, folder.as_deref());
            if !files.is_empty() && history::ran_commands(&dropped) {
                rows.push(history::COMMANDS_NOTE.to_owned());
            }
            (files, rows)
        });
        cx.spawn_in(window, async move |this, cx| {
            let (files, rows) = planned.await;
            let _ = this.update_in(cx, |this, window, cx| {
                if this.busy || this.store.conversations[this.store.active].id != session {
                    return;
                }
                this.ask_rewind(keep, back, files, rows, window, cx);
            });
        })
        .detach();
    }

    /// Ask whether to go back to `keep`, listing `files` as `rows` under the
    /// box that puts them back too.
    fn ask_rewind(
        &mut self,
        keep: usize,
        back: Option<String>,
        files: Vec<history::Restore>,
        rows: Vec<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let gone = self.open_messages().len().saturating_sub(keep);
        if gone == 0 {
            return;
        }
        let mut body = if back.is_some() {
            format!(
                "This message and {} after it leave the session, and its text goes back in the \
                 box.",
                message_count(gone - 1)
            )
        } else {
            format!("{} after this leave the session.", message_count(gone))
        };
        if files.is_empty() {
            body.push_str(" Files the agent changed stay as they are.");
        }
        let session = self.store.conversations[self.store.active].id;
        // Unticked to start with, as the terminal's /undo needs --files:
        // putting files back replaces what is on disk, so it is asked for.
        let put_back = Rc::new(Cell::new(false));
        let chat = cx.entity();
        window.open_dialog(cx, move |dialog, _, cx| {
            let (chat, back) = (chat.clone(), back.clone());
            let (files, put_back) = (files.clone(), put_back.clone());
            let ticked = put_back.get();
            dialog
                .title(div().font_family(HEADING_FONT).child("Rewind to here"))
                .w(px(460.))
                .confirm()
                .button_props(
                    DialogButtonProps::default()
                        .ok_text("Rewind")
                        .ok_variant(ButtonVariant::Danger),
                )
                .child(
                    div()
                        .text_sm()
                        .text_color(cx.theme().muted_foreground)
                        .child(body.clone()),
                )
                .when(!rows.is_empty(), |dialog| {
                    dialog.child(
                        v_flex()
                            .mt_3()
                            .gap_1()
                            .child(
                                Checkbox::new("rewind-files")
                                    .label("Put the files back too")
                                    .checked(ticked)
                                    .on_click({
                                        let put_back = put_back.clone();
                                        move |checked, window, _| {
                                            put_back.set(*checked);
                                            window.refresh();
                                        }
                                    }),
                            )
                            .children(rows.iter().map(|row| {
                                div()
                                    .pl_6()
                                    .text_sm()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(row.clone())
                            })),
                    )
                })
                .on_ok(move |_, window, cx| {
                    chat.update(cx, |this, cx| {
                        // A reply may have started, or the session changed,
                        // while the question was up.
                        if this.busy || this.store.conversations[this.store.active].id != session {
                            return;
                        }
                        this.cut_to(keep);
                        if put_back.get() && !files.is_empty() {
                            this.put_files_back(session, files.clone(), window, cx);
                        }
                        if let Some(text) = back.clone() {
                            this.composer
                                .update(cx, |input, cx| input.set_value(text, window, cx));
                        }
                        this.focus_composer(window, cx);
                        cx.notify();
                    });
                    true
                })
        });
    }

    /// Put back the files a rewind of `session` listed, off the UI thread,
    /// and say how it went: what could not be, and what was left alone, by
    /// name.
    fn put_files_back(
        &mut self,
        session: u64,
        files: Vec<history::Restore>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let folder = self.store.conversations[self.store.active]
            .workspace
            .clone();
        let restored = cx
            .background_executor()
            .spawn(async move { history::restore(session, &files, folder.as_deref()) });
        cx.spawn_in(window, async move |this, cx| {
            let done = restored.await;
            let said = done.lines().join("\n");
            let _ = this.update_in(cx, |_, window, cx| {
                if !done.left.is_empty() {
                    window.push_notification(Notification::warning(said), cx);
                } else if !said.is_empty() {
                    window.push_notification(Notification::success(said), cx);
                }
            });
        })
        .detach();
    }

    /// Carry on in a copy of the session up to this turn; the original stays
    /// as it is. From your message, the copy stops before it and its text is
    /// in the box, ready to be asked differently.
    pub(crate) fn fork_turn(&mut self, span: Span, window: &mut Window, cx: &mut Context<Self>) {
        let id = self.store.next_id();
        let back = span.yours.then(|| self.prompt_text(span.first)).flatten();
        let source = &self.store.conversations[self.store.active];
        let messages = source.messages[..span.keep().min(source.messages.len())].to_vec();
        let fork = Conversation {
            id,
            title: format!("{} (fork)", source.title),
            updated: backend::now(),
            workspace: source.workspace.clone(),
            todos: todos_in(&messages),
            messages,
            ..Default::default()
        };
        history::share(source.id, id);
        self.store.conversations.push(fork);
        self.store.active = self.store.conversations.len() - 1;
        self.page = Page::Chat;
        self.editing_message = None;
        self.save();
        self.composer.update(cx, |input, cx| {
            input.set_value(back.unwrap_or_default(), window, cx)
        });
        self.focus_composer(window, cx);
        self.follow();
        cx.notify();
    }

    /// The right-click menu for a turn: the same actions as its buttons.
    pub(crate) fn turn_menu(
        &self,
        span: Span,
        cx: &Context<Self>,
    ) -> impl Fn(PopupMenu, &mut Window, &mut Context<PopupMenu>) -> PopupMenu + 'static {
        let chat = cx.entity();
        let (retry, rewind) = self.can_retry_and_rewind(span);
        move |menu, _, _| {
            let item = |label: &'static str, icon: Icon, enabled: bool, act: Act| {
                let chat = chat.clone();
                PopupMenuItem::new(label)
                    .icon(icon)
                    .disabled(!enabled)
                    .on_click(move |_, window, cx| {
                        chat.update(cx, |this, cx| act(this, span, window, cx))
                    })
            };
            let mut menu = menu.min_w(px(200.)).item(item(
                "Copy",
                Icon::new(IconName::Copy),
                true,
                Chat::copy_turn,
            ));
            if span.yours {
                menu = menu.item(item(
                    "Edit",
                    Icon::new(AppIcon::Pencil),
                    retry,
                    Chat::edit_turn,
                ));
            }
            menu.item(item(
                "Retry",
                Icon::new(IconName::Redo),
                retry,
                Chat::retry_turn,
            ))
            .separator()
            .item(item(
                "Fork from here",
                Icon::new(AppIcon::Branch),
                true,
                Chat::fork_turn,
            ))
            .item(item(
                "Rewind to here",
                Icon::new(IconName::Undo),
                rewind,
                Chat::rewind_turn,
            ))
        }
    }

    /// The row of small buttons under a turn. It shows while the turn is
    /// hovered, and always under the newest reply, which is the one most
    /// often asked again.
    pub(crate) fn turn_bar(&self, span: Span, always: bool, cx: &Context<Self>) -> AnyElement {
        let (retry, rewind) = self.can_retry_and_rewind(span);
        let button =
            |name: &'static str, icon: Icon, tip: &'static str, enabled: bool, act: Act| {
                Button::new(self.row_id(name, span.first))
                    .ghost()
                    .xsmall()
                    .icon(icon)
                    .tooltip(tip)
                    .disabled(!enabled)
                    .on_click(cx.listener(move |this, _, window, cx| act(this, span, window, cx)))
            };
        h_flex()
            .gap_0p5()
            .items_center()
            // Over your card it sits on the text, so it needs a ground.
            .when(span.yours, |bar| {
                bar.px_0p5()
                    .rounded(cx.theme().radius)
                    .bg(cx.theme().background)
                    .border_1()
                    .border_color(cx.theme().border)
            })
            .when(!always, |bar| {
                bar.invisible().group_hover("turn", |style| style.visible())
            })
            .when(span.yours, |bar| {
                bar.child(button(
                    "edit",
                    Icon::new(AppIcon::Pencil),
                    "Edit",
                    retry,
                    Chat::edit_turn,
                ))
                .child(button(
                    "copy-yours",
                    Icon::new(IconName::Copy),
                    "Copy",
                    true,
                    Chat::copy_turn,
                ))
            })
            .child(button(
                "retry",
                Icon::new(IconName::Redo),
                "Retry",
                retry,
                Chat::retry_turn,
            ))
            .child(button(
                "fork",
                Icon::new(AppIcon::Branch),
                "Fork from here",
                true,
                Chat::fork_turn,
            ))
            .child(button(
                "rewind",
                Icon::new(IconName::Undo),
                "Rewind to here",
                rewind,
                Chat::rewind_turn,
            ))
            .into_any_element()
    }

    /// Whether the turn can be asked again, and gone back to. Neither while
    /// a reply runs, which owns the session until it stops.
    fn can_retry_and_rewind(&self, span: Span) -> (bool, bool) {
        let retry = !self.busy
            && span
                .prompt()
                .is_some_and(|at| self.prompt_text(at).is_some());
        let rewind = !self.busy && (span.yours || self.open_messages().len() > span.keep());
        (retry, rewind)
    }
}

/// The checklist the last `todo` call in `messages` set, or none.
fn todos_in(messages: &[Message]) -> Vec<Todo> {
    messages
        .iter()
        .rev()
        .flat_map(|m| m.tool_calls.iter().rev())
        .find(|call| call.name == letronna_core::tools::TODO)
        .and_then(|call| parse_todos(&call.arguments).ok())
        .unwrap_or_default()
}

/// "1 message" or "N messages".
fn message_count(n: usize) -> String {
    if n == 1 {
        "1 message".to_owned()
    } else {
        format!("{n} messages")
    }
}

#[cfg(test)]
mod tests {
    // Not `use super::*`: the prelude re-exports gpui, whose `test` macro
    // would shadow the built-in attribute.
    use super::{Span, message_count, todos_in};
    use letronna_core::chat::{Message, ToolCall};

    #[test]
    fn a_turn_keeps_what_comes_before_your_message_or_up_to_its_reply() {
        let yours = Span {
            first: 4,
            last: 4,
            yours: true,
        };
        assert_eq!((yours.prompt(), yours.keep()), (Some(4), 4));
        let reply = Span {
            first: 5,
            last: 8,
            yours: false,
        };
        assert_eq!((reply.prompt(), reply.keep()), (Some(4), 9));
        let opening = Span {
            first: 0,
            last: 2,
            yours: false,
        };
        assert_eq!(opening.prompt(), None);
    }

    #[test]
    fn the_checklist_is_the_last_one_left_standing() {
        let todo = |items: &str| Message {
            role: "assistant".into(),
            tool_calls: vec![ToolCall {
                id: "call".into(),
                name: letronna_core::tools::TODO.into(),
                arguments: format!(r#"{{"items":[{items}]}}"#),
            }],
            ..Default::default()
        };
        let first = todo(r#"{"text":"plan","status":"done"}"#);
        let second = todo(r#"{"text":"build","status":"doing"}"#);
        assert_eq!(todos_in(&[first.clone(), second])[0].text, "build");
        assert_eq!(todos_in(&[first])[0].text, "plan");
        assert!(todos_in(&[]).is_empty());
    }

    #[test]
    fn counts_read_as_words() {
        assert_eq!(message_count(1), "1 message");
        assert_eq!(message_count(3), "3 messages");
    }
}
