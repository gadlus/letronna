//! What can be done with a turn once it is there: copy it, change your
//! message and ask again, ask the same again, go back to it, or carry on
//! from it in a copy of the session. Each turn offers these in a row of
//! small buttons and on right-click.
//!
//! Everything works by place in the session. Going back is saved as a
//! rewind record the store appends (`Conversation::rewind`), so the terminal
//! sees the shorter session too.

use crate::prelude::*;
use gpui_component::menu::PopupMenu;
use gpui_component::notification::Notification;
use gpui_component::{button::ButtonVariant, dialog::DialogButtonProps};

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
        // What had the keys, the edit box or the turn's buttons, is gone.
        self.focus_composer(window, cx);
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
        let gone = self.open_messages().len().saturating_sub(keep);
        if gone == 0 {
            return;
        }
        let back = span.yours.then(|| self.prompt_text(span.first)).flatten();
        let body = if back.is_some() {
            format!(
                "This message and {} after it leave the session, and its text goes back in the \
                 box. Files the agent changed stay as they are.",
                message_count(gone - 1)
            )
        } else {
            format!(
                "{} after this leave the session. Files the agent changed stay as they are.",
                message_count(gone)
            )
        };
        let session = self.store.conversations[self.store.active].id;
        let chat = cx.entity();
        window.open_dialog(cx, move |dialog, _, cx| {
            let (chat, back) = (chat.clone(), back.clone());
            dialog
                .title(div().font_family(HEADING_FONT).child("Rewind to here"))
                .w(px(420.))
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
                .on_ok(move |_, window, cx| {
                    chat.update(cx, |this, cx| {
                        // A reply may have started, or the session changed,
                        // while the question was up.
                        if this.busy || this.store.conversations[this.store.active].id != session {
                            return;
                        }
                        this.cut_to(keep);
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

    /// What lets a turn be picked out with Tab, kept per session and place
    /// so it lives as long as the turn is drawn.
    pub(crate) fn turn_focus(
        &self,
        first: usize,
        window: &mut Window,
        cx: &mut App,
    ) -> FocusHandle {
        window
            .use_keyed_state(self.row_id("turn-focus", first), cx, |_, cx| {
                cx.focus_handle().tab_stop(true)
            })
            .read(cx)
            .clone()
    }

    /// The row of small buttons for a turn, always laid out so nothing moves
    /// when it shows, and drawn only where `shows_bar` says.
    pub(crate) fn turn_bar(&self, span: Span, shown: bool, cx: &Context<Self>) -> AnyElement {
        let (retry, rewind) = self.can_retry_and_rewind(span);
        let button =
            |name: &'static str, icon: Icon, tip: &'static str, enabled: bool, act: Act| {
                Button::new(self.row_id(name, span.first))
                    .plain(cx)
                    .xsmall()
                    .icon(icon)
                    .tooltip(tip)
                    .disabled(!enabled)
                    .on_click(cx.listener(move |this, _, window, cx| act(this, span, window, cx)))
            };
        h_flex()
            .gap_0p5()
            .items_center()
            .when(!shown, |bar| bar.invisible())
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

/// Whether a turn shows its buttons: the newest one once nothing is coming
/// in, since it is the one most often asked again, and the one that holds
/// focus, picked out with Tab or by selecting its text.
pub(crate) fn shows_bar(span: Span, messages: usize, streaming: bool, focused: bool) -> bool {
    focused || (!streaming && span.last + 1 == messages)
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
    use super::{Span, message_count, shows_bar, todos_in};
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
    fn the_bar_shows_under_the_newest_turn_and_the_focused_one() {
        let turn = |first, last, yours| Span { first, last, yours };
        let (prompt, reply) = (turn(4, 4, true), turn(5, 8, false));
        // Nine messages, the reply last: only it shows, until one is picked.
        assert!(shows_bar(reply, 9, false, false));
        assert!(!shows_bar(prompt, 9, false, false));
        assert!(shows_bar(prompt, 9, false, true));
        // A prompt nothing answered is the newest turn itself.
        assert!(shows_bar(prompt, 5, false, false));
        // While a reply comes in, the prompt it answers waits to be picked.
        assert!(!shows_bar(prompt, 5, true, false));
        assert!(shows_bar(prompt, 5, true, true));
        // An earlier reply stays bare until it is picked.
        assert!(!shows_bar(turn(0, 3, false), 9, false, false));
    }

    #[test]
    fn counts_read_as_words() {
        assert_eq!(message_count(1), "1 message");
        assert_eq!(message_count(3), "3 messages");
    }
}
