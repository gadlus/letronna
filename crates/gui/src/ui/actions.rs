//! What can be done with a turn once it is there: copy it, change your
//! message and ask again, ask the same again, go back to it, carry on from
//! it in a copy of the session, or have everything up to it summarized.
//! Each turn offers these in a row of small buttons and on right-click, and
//! to the keyboard once Alt+Up or Alt+Down has stepped to it.
//!
//! Everything works by place in the session. Going back is saved as a
//! rewind record the store appends (`Conversation::rewind`), so the terminal
//! sees the shorter session too. It can also put back the files the agent's
//! tools wrote since, from the copies `history` kept of them.
//!
//! The newest finished turn ends with one line of the files it changed and
//! the lines it added and removed, the Changes tab a key away, and a button
//! that puts them all back without leaving the conversation. A turn that
//! changed only a few files lists them under the line, each diff a key
//! away. What the card lists is read off the UI thread once per session and
//! length, never on a timer.

use crate::app::Given;
use crate::prelude::*;
use gpui_component::checkbox::Checkbox;
use gpui_component::menu::PopupMenu;
use gpui_component::notification::Notification;
use gpui_component::{button::ButtonVariant, dialog::DialogButtonProps};
use letronna_core::history;
use std::{cell::Cell, collections::HashMap, rc::Rc};

actions!(turn_files, [ToggleDiff, ReviewFiles]);

/// Diff lines drawn under a file at most; a file rewritten whole says how
/// many more rather than laying out thousands of rows.
const DIFF_ROWS: usize = 300;

/// Characters of one diff line drawn at most: a minified bundle is one line
/// of megabytes, copied again on every frame were it drawn whole.
const DIFF_LINE: usize = 500;

/// Files the card lists under its line at most. A turn that changed more
/// rests as the line alone, and Review shows them all in the Changes tab.
const CARD_FILES: usize = 3;

/// Diff lines an open diff shows before it scrolls, and half of one more
/// under them, cut, to say there is more: a file rewritten whole would
/// otherwise push the conversation thousands of pixels down.
const DIFF_SHOWN: f32 = 12.5;

actions!(
    turn,
    [
        PreviousTurn,
        NextTurn,
        CopyTurn,
        EditTurn,
        RetryTurn,
        RewindTurn,
        ForkTurn,
        TurnMenu
    ]
);

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

/// The files the newest finished turn of a session changed, as last read.
pub(crate) struct TurnFiles {
    /// The session and its length when they were read: any new message,
    /// or a rewind, means another read.
    key: (u64, usize),
    /// Where the turn's reply starts.
    first: usize,
    files: Vec<history::Changed>,
    /// Each file's path as the card shows it, worked out with the files:
    /// it resolves the project folder on disk, which a frame cannot wait on.
    labels: Vec<String>,
    /// Whether the turn ran commands, whose changes are not put back.
    ran_commands: bool,
    /// Makes each file a tab stop, so Enter can open its diff.
    focus: Vec<FocusHandle>,
    /// The files whose diffs are open, by place in `files`, each with where
    /// its diff is scrolled to.
    open: HashMap<usize, ScrollHandle>,
    /// Makes the card's line a tab stop, so Enter can open the review.
    line: FocusHandle,
}

/// An action on a turn, as the menu and the buttons both call it.
type Act = fn(&mut Chat, Span, &mut Window, &mut Context<Chat>);

/// A place Alt+Up and Alt+Down stop at.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum Stop {
    /// A turn with a menu, by its first message.
    Turn(usize),
    /// The question or the permission card waiting on the user, at the end.
    Question,
}

impl Chat {
    fn open_messages(&self) -> &[Message] {
        &self.store.conversations[self.store.active].messages
    }

    /// The text of your message at `at`, if that is what is there, with
    /// `/init`, a saved workflow's command and `@` references as they were
    /// typed.
    fn prompt_text(&self, at: usize) -> Option<String> {
        self.open_messages()
            .get(at)
            .filter(|m| m.role == "user" && !m.notice)
            .map(|m| {
                let content = letronna_core::references::typed(&m.content);
                let typed = letronna_core::init::typed(content).map(str::to_owned);
                typed
                    .or_else(|| letronna_core::workflow::saved::typed(content))
                    .unwrap_or_else(|| content.to_owned())
            })
    }

    /// The pictures your message at `at` carried, to go with it again.
    fn prompt_images(&self, at: usize) -> Vec<PathBuf> {
        self.open_messages()
            .get(at)
            .map(|m| m.images.clone())
            .unwrap_or_default()
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
        // A review of what is taken back must not keep any of it.
        self.stop_review(id);
        let conversation = &mut self.store.conversations[ix];
        conversation.rewind(keep);
        // The checklist is the one the last `todo` left standing.
        conversation.todos = todos_in(&conversation.messages);
        self.thinking_open
            .retain(|&(session, at)| session != id || at < keep);
        self.runs_open
            .retain(|&(session, at)| session != id || at < keep);
        self.previews_open
            .retain(|&(session, at)| session != id || at < keep);
        self.thought_for
            .retain(|&(session, at), _| session != id || at < keep);
        self.edited
            .retain(|&(session, at), _| session != id || at < keep);
        self.save();
    }

    pub(crate) fn copy_turn(&mut self, span: Span, window: &mut Window, cx: &mut Context<Self>) {
        cx.write_to_clipboard(ClipboardItem::new_string(self.turn_text(span)));
        window.push_notification(Notification::success("Copied."), cx);
    }

    /// Change your message where it is. Nothing is taken back until it is
    /// sent, so looking and cancelling costs nothing.
    pub(crate) fn edit_turn(&mut self, span: Span, window: &mut Window, cx: &mut Context<Self>) {
        if !self.idle() || self.run_holds_open() {
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
        if !self.idle() || self.run_holds_open() {
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
        let images = self.prompt_images(at);
        self.back_to(at, window, cx);
        self.ask_again(text, images, window, cx);
    }

    /// Whether you asked anything after your message at `at`, which asking
    /// that message again would take away.
    pub(crate) fn asked_after(&self, at: usize) -> bool {
        self.open_messages()
            .iter()
            .skip(at + 1)
            .any(|m| m.role == "user")
    }

    /// Go back to just before your message at `at`, to ask it again. When
    /// you asked more after it, that stays: the asking goes to a fork.
    fn back_to(&mut self, at: usize, window: &mut Window, cx: &mut Context<Self>) {
        if !self.asked_after(at) {
            self.cut_to(at);
            return;
        }
        self.fork_at(at, window, cx);
        window.push_notification(
            Notification::info("Asked in a fork. The original keeps what came after."),
            cx,
        );
    }

    /// Ask the turn's prompt again, dropping what came of it last time.
    pub(crate) fn retry_turn(&mut self, span: Span, window: &mut Window, cx: &mut Context<Self>) {
        if !self.idle() || self.run_holds_open() {
            return;
        }
        let Some(at) = span.prompt() else {
            return;
        };
        let Some(text) = self.prompt_text(at) else {
            return;
        };
        let images = self.prompt_images(at);
        self.back_to(at, window, cx);
        self.ask_again(text, images, window, cx);
    }

    fn ask_again(
        &mut self,
        text: String,
        images: Vec<PathBuf>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // What had the keys, the edit box or the turn's buttons, is gone.
        self.focus_composer(window, cx);
        // A saved workflow's command runs again as it did, granted its turn.
        if let Some(Ok(Given::Run(asked))) = self.added_command(&text) {
            if self.run_saved(&text, asked, cx).is_err() {
                self.composer
                    .update(cx, |input, cx| input.set_value(text, window, cx));
                self.open_settings(window, cx);
            }
            return;
        }
        let Some(prompt) = self.prompt_for(text.clone(), cx) else {
            self.composer
                .update(cx, |input, cx| input.set_value(text, window, cx));
            return;
        };
        let session = self.store.conversations[self.store.active].id;
        crate::app::tell_crew(session, &text, letronna_core::crew::Source::Composer);
        if self.dispatch(prompt, images, None, cx).is_err() {
            // Not asked, so the text waits in the box rather than being lost.
            self.composer
                .update(cx, |input, cx| input.set_value(text, window, cx));
            self.open_settings(window, cx);
        }
    }

    /// Go back to this turn, once asked: what follows it leaves the session.
    /// From your message, its text comes back to the box, as in Claude Code.
    pub(crate) fn rewind_turn(&mut self, span: Span, window: &mut Window, cx: &mut Context<Self>) {
        if !self.idle() {
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
                if !this.idle() || this.store.conversations[this.store.active].id != session {
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
                        // A reply or a summary may have started, or the
                        // session changed, while the question was up.
                        let open = this.store.conversations[this.store.active].id;
                        if !this.idle() || this.run_holds_open() || open != session {
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
    pub(crate) fn put_files_back(
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
            let _ = this.update_in(cx, |this, window, cx| {
                // The files are not what the card read any more.
                this.turn_files_for = None;
                cx.notify();
                if !done.left.is_empty() {
                    window.push_notification(Notification::warning(said), cx);
                } else if !said.is_empty() {
                    window.push_notification(Notification::success(said), cx);
                }
                // The Changes tab and the status bar count what went back.
                this.refresh_changes(cx);
            });
        })
        .detach();
    }

    /// Carry on in a copy of the session up to this turn; the original stays
    /// as it is. From your message, the copy stops before it and its text is
    /// in the box, ready to be asked differently.
    pub(crate) fn fork_turn(&mut self, span: Span, window: &mut Window, cx: &mut Context<Self>) {
        let back = span.yours.then(|| self.prompt_text(span.first)).flatten();
        self.fork_at(span.keep(), window, cx);
        self.composer.update(cx, |input, cx| {
            input.set_value(back.unwrap_or_default(), window, cx)
        });
        self.focus_composer(window, cx);
        cx.notify();
    }

    /// Summarize the session up to this turn now: everything before your
    /// message, or up to the end of the reply, as far as a turn starts
    /// there. The turns after stay word for word.
    pub(crate) fn summarize_turn(
        &mut self,
        span: Span,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.idle() {
            return;
        }
        let mut connection = match self.connection_for(self.store.active, None) {
            Ok(connection) => connection,
            Err(error) => {
                window.push_notification(Notification::error(error), cx);
                return;
            }
        };
        let open = &self.store.conversations[self.store.active];
        // What the system prompt says of the folder counts toward the sizes.
        connection.workspace = open.workspace.clone();
        let (session, messages, floor) = (open.id, open.messages.clone(), open.floor());
        let summary = open.summary.as_ref().map(|(_, text)| text.clone());
        let upto = span.keep();
        self.compacting.insert(session);
        // Up to a minute's model call, so on a thread of its own.
        let (sender, receiver) = async_channel::bounded(1);
        std::thread::spawn(move || {
            let done = backend::compact(&connection, &messages, floor, summary.as_deref(), upto);
            let _ = sender.send_blocking(done);
        });
        window.push_notification(Notification::info("Summarizing the earlier turns…"), cx);
        cx.spawn_in(window, async move |this, cx| {
            let done = receiver
                .recv()
                .await
                .unwrap_or_else(|_| Err("The summary stopped part way.".into()));
            let _ = this.update_in(cx, |this, window, cx| {
                this.compacting.remove(&session);
                // Kept only by the session it was made of, as it still starts.
                let kept = done.and_then(|done| {
                    let report = done.report();
                    let ix = this.store.index_of(session).ok_or("The session is gone.")?;
                    this.store.conversations[ix].compacted(done)?;
                    Ok(report)
                });
                match kept {
                    Ok(report) => {
                        this.save();
                        window.push_notification(Notification::success(report), cx);
                    }
                    Err(why) => window.push_notification(Notification::warning(why), cx),
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    /// Open a copy of the session's first `keep` messages, leaving the
    /// session itself as it is.
    fn fork_at(&mut self, keep: usize, window: &mut Window, cx: &mut Context<Self>) {
        let id = self.store.reserve_id();
        let source = &self.store.conversations[self.store.active];
        let messages = source.messages[..keep.min(source.messages.len())].to_vec();
        let mut fork = Conversation {
            id,
            title: format!("{} (fork)", source.title),
            updated: backend::now(),
            workspace: source.workspace.clone(),
            todos: todos_in(&messages),
            model: source.model.clone(),
            gateway: source.gateway.clone(),
            effort: source.effort.clone(),
            messages,
            ..Default::default()
        };
        fork.copy_summaries(source);
        history::share(source.id, id);
        self.leave_for(self.store.conversations.len());
        self.store.conversations.push(fork);
        self.store.active = self.store.conversations.len() - 1;
        self.page = Page::Chat;
        self.editing_message = None;
        self.save();
        self.swap_draft(window, cx);
        self.follow();
    }

    /// The right-click menu for a turn: the same actions as its buttons.
    pub(crate) fn turn_menu(
        &self,
        span: Span,
        cx: &Context<Self>,
    ) -> impl Fn(PopupMenu, &mut Window, &mut Context<PopupMenu>) -> PopupMenu + 'static {
        let chat = cx.entity();
        let (retry, rewind) = self.can_retry_and_rewind(span);
        let summarize = self.can_summarize(span);
        let retry_label = self.retry_label(span);
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
                retry_label,
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
            .item(item(
                "Summarize up to here",
                Icon::new(AppIcon::FileText),
                summarize,
                Chat::summarize_turn,
            ))
        }
    }

    /// Where Alt+Up and Alt+Down stop in the session on screen, oldest
    /// first. Read from its turns rather than from what is drawn, as the
    /// list draws only the turns on screen.
    pub(crate) fn stops(&self) -> Vec<Stop> {
        let conversation = &self.store.conversations[self.store.active];
        let streaming_here = self.busy && self.streaming_id == Some(conversation.id);
        let asking = streaming_here && self.card_takes_keys();
        stops(&self.turns, &conversation.messages, streaming_here, asking)
    }

    /// Step from `from` to the stop before, or with `down` the one after;
    /// from outside the stops, to the newest.
    pub(crate) fn step_turn(
        &mut self,
        from: Option<Stop>,
        down: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.page != Page::Chat
            || !self.transcript_shown()
            || self.panel_covers(window, cx)
            || self.settings_open
            || window.has_active_dialog(cx)
        {
            return;
        }
        let stops = self.stops();
        let at = from.and_then(|from| stops.iter().position(|&stop| stop == from));
        let Some(next) = step_stop(stops.len(), at, down) else {
            return;
        };
        self.go_to_stop(stops[next], true, window);
        self.region = Some(crate::ui::keys::Region::Transcript);
        cx.notify();
    }

    /// Give `stop` the keys, and with `reveal` scroll a turn into sight. A
    /// turn's focus lives only while the list draws it, so the transcript
    /// holds the keys until `turn_row` hands them over at the next draw.
    pub(crate) fn go_to_stop(&mut self, stop: Stop, reveal: bool, window: &mut Window) {
        match stop {
            Stop::Turn(first) => {
                let session = self.store.conversations[self.store.active].id;
                window.focus(&self.transcript_focus);
                self.focus_turn = Some((session, first));
                // Held there as a search hit is, until scrolled by hand.
                if reveal {
                    self.following = false;
                    self.reveal = Some((session, first));
                    self.revealed = None;
                    self.hit_at = None;
                }
            }
            // Always brought into sight: the card is at the end, where the
            // newest is, and out of sight it could not be answered.
            Stop::Question => {
                window.focus(&self.question_focus);
                self.follow();
            }
        }
    }

    /// `turn` answering its keys: C copies, E edits, R retries, W rewinds,
    /// F forks and Enter opens its menu; Alt+Up and Alt+Down step from it.
    pub(crate) fn turn_keys(
        &self,
        turn: Stateful<Div>,
        span: Span,
        focus: FocusHandle,
        cx: &Context<Self>,
    ) -> Stateful<Div> {
        let here = Some(Stop::Turn(span.first));
        turn.key_context("Turn")
            .on_action(cx.listener(move |this, _: &PreviousTurn, window, cx| {
                this.step_turn(here, false, window, cx)
            }))
            .on_action(cx.listener(move |this, _: &NextTurn, window, cx| {
                this.step_turn(here, true, window, cx)
            }))
            .on_action(Self::turn_key::<CopyTurn>(
                &focus,
                span,
                Chat::copy_turn,
                cx,
            ))
            .on_action(Self::turn_key::<EditTurn>(
                &focus,
                span,
                Chat::edit_turn,
                cx,
            ))
            .on_action(Self::turn_key::<RetryTurn>(
                &focus,
                span,
                Chat::retry_turn,
                cx,
            ))
            .on_action(Self::turn_key::<RewindTurn>(
                &focus,
                span,
                Chat::rewind_turn,
                cx,
            ))
            .on_action(Self::turn_key::<ForkTurn>(
                &focus,
                span,
                Chat::fork_turn,
                cx,
            ))
            .on_action(Self::turn_key::<TurnMenu>(
                &focus,
                span,
                Chat::open_turn_menu,
                cx,
            ))
    }

    /// A turn's key, which acts only while the turn itself has the focus:
    /// in the box your message is changed in, the key is typed instead.
    fn turn_key<A: Action>(
        focus: &FocusHandle,
        span: Span,
        act: Act,
        cx: &Context<Self>,
    ) -> impl Fn(&A, &mut Window, &mut App) + 'static {
        let focus = focus.clone();
        cx.listener(move |this, _: &A, window, cx| {
            if !focus.is_focused(window) {
                return cx.propagate();
            }
            act(this, span, window, cx)
        })
    }

    /// The turn's menu, opened from the keyboard under its buttons, with
    /// the keys in it.
    fn open_turn_menu(&mut self, span: Span, window: &mut Window, cx: &mut Context<Self>) {
        // turn_key calls this only while the turn has the focus, and
        // turn_focus's keyed state can be read only while drawing: from a
        // key it aborted the window.
        let Some(turn) = window.focused(cx) else {
            return;
        };
        let build = self.turn_menu(span, cx);
        let menu = PopupMenu::build(window, cx, build);
        menu.focus_handle(cx).focus(window);
        cx.subscribe_in(
            &menu,
            window,
            move |this, menu, _: &DismissEvent, window, cx| {
                // Escape gives the keys back to the turn; an item that moved
                // them, to the composer or a dialog, keeps them there.
                if menu.focus_handle(cx).contains_focused(window, cx) {
                    window.focus(&turn);
                }
                this.turn_menu = None;
                cx.notify();
            },
        )
        .detach();
        // The menu closes only on Escape, a click outside or an item, so
        // F6, Alt+Up or Ctrl+L left it hung open under the turn, for the
        // same place in whichever session was opened next.
        let left = cx.on_focus_out(&menu.focus_handle(cx), window, |this, _, _, cx| {
            this.turn_menu = None;
            cx.notify();
        });
        self.turn_menu = Some((span.first, menu, left));
        cx.notify();
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

    /// The row of small buttons for a turn where `shows_bar` says. Other
    /// turns get one quiet button with the same actions in a menu, so an
    /// earlier turn still shows that it has them.
    pub(crate) fn turn_bar(&self, span: Span, shown: bool, cx: &Context<Self>) -> AnyElement {
        let bar = self.bar_buttons(span, shown, cx);
        let Some((_, menu, _)) = self
            .turn_menu
            .as_ref()
            .filter(|(first, _, _)| *first == span.first)
        else {
            return bar;
        };
        v_flex()
            .child(bar)
            .child(
                deferred(
                    anchored()
                        .snap_to_window_with_margin(px(8.))
                        .child(menu.clone()),
                )
                .with_priority(1),
            )
            .into_any_element()
    }

    /// The buttons, or the one quiet button, without a menu hung from them.
    fn bar_buttons(&self, span: Span, shown: bool, cx: &Context<Self>) -> AnyElement {
        if !shown {
            // Your prompt's sits at the right end of its label.
            let corner = if span.yours {
                gpui::Corner::TopRight
            } else {
                gpui::Corner::TopLeft
            };
            return Button::new(self.row_id("more", span.first))
                .plain(cx)
                .xsmall()
                .icon(Icon::new(IconName::Ellipsis))
                .text_color(cx.theme().muted_foreground)
                .tooltip("More")
                .dropdown_menu_with_anchor(corner, self.turn_menu(span, cx))
                .into_any_element();
        }
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
                self.retry_label(span),
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
            .child(button(
                "summarize",
                Icon::new(AppIcon::FileText),
                "Summarize up to here",
                self.can_summarize(span),
                Chat::summarize_turn,
            ))
            .into_any_element()
    }

    /// Whether the turn can be asked again, and gone back to. Neither while
    /// a reply runs, which owns the session until it stops, nor while a
    /// summary is made.
    fn can_retry_and_rewind(&self, span: Span) -> (bool, bool) {
        let retry = self.idle()
            && span
                .prompt()
                .is_some_and(|at| self.prompt_text(at).is_some());
        let rewind = self.idle() && (span.yours || self.open_messages().len() > span.keep());
        (retry, rewind)
    }

    /// Whether summing up to the turn would sum anything up: a turn comes
    /// before it that the user has not had summarized already. Nor while
    /// something holds the session.
    fn can_summarize(&self, span: Span) -> bool {
        let open = &self.store.conversations[self.store.active];
        self.idle() && backend::summarizable(&open.messages, open.floor(), span.keep())
    }

    /// Whether nothing holds the session: no reply running, and no summary
    /// being made, which a rewind or another summary would cut under.
    fn idle(&self) -> bool {
        let open = self.store.conversations[self.store.active].id;
        !self.busy && !self.compacting.contains(&open)
    }

    /// What Retry does here: ask again in place, or in a fork when more was
    /// asked after this turn.
    fn retry_label(&self, span: Span) -> &'static str {
        if span.prompt().is_some_and(|at| self.asked_after(at)) {
            "Retry in a fork"
        } else {
            "Retry"
        }
    }
}

impl Chat {
    /// Read what the newest finished turn of the open session changed,
    /// once for each session and length; a turn that wrote no file with
    /// the file tools costs no read at all.
    pub(crate) fn watch_turn_files(&mut self, cx: &mut Context<Self>) {
        let open = &self.store.conversations[self.store.active];
        let key = (open.id, open.messages.len());
        if self.busy && self.streaming_id == Some(open.id) {
            // Asked again, a turn can end at the length of the one it
            // replaced, whose card would then pass for its own until read.
            self.show_turn_files(None);
            return;
        }
        if self.turn_files_for == Some(key) {
            return;
        }
        self.turn_files_for = Some(key);
        let first = open
            .messages
            .iter()
            .rposition(|m| m.role == "user")
            .map_or(0, |at| at + 1);
        if open.messages[first..].iter().all(|m| m.files.is_empty()) {
            self.show_turn_files(None);
            return;
        }
        let turn = open.messages[first..].to_vec();
        let folder = open.workspace.clone();
        let read = cx.background_executor().spawn(async move {
            let files = history::changed(key.0, &turn, folder.as_deref());
            let labels = files
                .iter()
                .map(|file| history::label(&file.restore.path, folder.as_deref()))
                .collect();
            (files, labels, history::ran_commands(&turn))
        });
        cx.spawn(async move |this, cx| {
            let (files, labels, ran_commands) = read.await;
            let _ = this.update(cx, |this, cx| {
                // Another read has started since, for what is there now.
                if this.turn_files_for != Some(key) {
                    return;
                }
                let card = (!files.is_empty()).then(|| TurnFiles {
                    key,
                    first,
                    focus: files
                        .iter()
                        .map(|_| cx.focus_handle().tab_stop(true))
                        .collect(),
                    files,
                    labels,
                    ran_commands,
                    open: HashMap::new(),
                    line: cx.focus_handle().tab_stop(true),
                });
                this.show_turn_files(card);
                cx.notify();
            });
        })
        .detach();
    }

    /// Put `card` after its turn in place of the one there, and have the
    /// chat's list measure both turns again: it keeps the height a turn off
    /// screen was last drawn at, which the card arriving or going changes.
    fn show_turn_files(&mut self, card: Option<TurnFiles>) {
        let was = std::mem::replace(&mut self.turn_files, card);
        // The list holds one session; a card read for another has no turn
        // in it.
        let turns = [&was, &self.turn_files]
            .into_iter()
            .flatten()
            .filter(|card| card.key.0 == self.listed.0)
            .filter_map(|card| {
                let mut turns = self.turns.iter();
                turns.position(|turn| turn.first == card.first && !turn.yours)
            });
        for n in turns {
            self.list.splice(n..n + 1, 1);
        }
    }

    /// The card that ends the turn at `span` when it is the newest finished
    /// one and changed files: one line of how many and how much, whose
    /// Review opens the Changes tab, and Undo; under it each file and its
    /// diff when open, when there are few enough to list.
    pub(crate) fn files_card(&self, span: Span, cx: &mut Context<Self>) -> Option<AnyElement> {
        let open = &self.store.conversations[self.store.active];
        let card = self
            .turn_files
            .as_ref()
            .filter(|card| card.key == (open.id, open.messages.len()))
            .filter(|card| !span.yours && card.first == span.first)?;
        let undoable = card.files.iter().any(|file| file.restore.fate.touches());
        let heading = match card.files.len() {
            1 => "1 file changed".to_owned(),
            n => format!("{n} files changed"),
        };
        let totals = line_totals(card.files.iter().map(|file| file.lines.as_ref()));
        let listed = card.files.len() <= CARD_FILES;
        let rows: Vec<_> = card
            .files
            .iter()
            .enumerate()
            .filter(|_| listed)
            .map(|(n, file)| {
                let scroll = card.open.get(&n);
                let path = &card.labels[n];
                v_flex()
                    .w_full()
                    .border_t_1()
                    .border_color(cx.theme().border)
                    .child(self.changed_file_row(
                        n,
                        file,
                        path,
                        scroll.is_some(),
                        &card.focus[n],
                        cx,
                    ))
                    .when_some(file.lines.as_ref().zip(scroll), |row, (lines, scroll)| {
                        row.child(diff_rows(self.row_id("file-diff", n), lines, scroll, cx))
                    })
            })
            .collect();
        let session = open.id;
        let files: Vec<history::Restore> =
            card.files.iter().map(|file| file.restore.clone()).collect();
        let theme = cx.theme();
        // What Undo leaves alone is said where Undo is: the card rests as
        // one line.
        let undo_tip = if card.ran_commands {
            format!(
                "Put these files back as they were before this turn. {}",
                history::COMMANDS_NOTE
            )
        } else {
            "Put these files back as they were before this turn".to_owned()
        };
        // The line's own part, apart from Undo: a press on Undo would
        // otherwise open the review as well.
        let summary = h_flex()
            .id(self.row_id("review-files", span.first))
            .flex_1()
            .min_w_0()
            .h_full()
            .pl_2()
            .gap_2()
            .items_center()
            .rounded(theme.radius_lg)
            .cursor_pointer()
            .active(|s| s.bg(theme.secondary.opacity(0.5)))
            .text_xs()
            .child(
                div()
                    .flex_none()
                    .font_family(HEADING_FONT)
                    .font_weight(FontWeight::MEDIUM)
                    .child(heading),
            )
            .when_some(totals, |line, (added, removed)| {
                line.child(
                    h_flex()
                        .flex_none()
                        .gap_1()
                        .child(div().text_color(theme.green).child(format!("+{added}")))
                        .child(div().text_color(theme.danger).child(format!("−{removed}"))),
                )
            })
            .child(
                div()
                    .flex_none()
                    .text_color(theme.muted_foreground)
                    .child("·"),
            )
            .child(
                div()
                    .min_w_0()
                    .truncate()
                    .text_color(theme.link)
                    .child("Review"),
            )
            // Enter is ReviewFiles', on the line that has the focus.
            .on_click(cx.listener(|this, event: &ClickEvent, window, cx| {
                if !event.is_keyboard() {
                    this.review_files(window, cx);
                }
            }));
        let line = h_flex()
            .id(self.row_id("files-line", span.first))
            .track_focus(&card.line)
            .key_context("TurnFiles")
            .h(px(30.))
            .w_full()
            .min_w_0()
            .pr_2()
            .gap_2()
            .items_center()
            .rounded(theme.radius_lg)
            .focus(|s| s.bg(theme.secondary))
            // As on a tool row, a press leaves the keys in the composer;
            // Tab is what picks the line out.
            .on_any_mouse_down(|_, window, _| window.prevent_default())
            // Only the line's own: Enter on Undo presses Undo.
            .on_action({
                let line = card.line.clone();
                cx.listener(move |this, _: &ReviewFiles, window, cx| {
                    if !line.is_focused(window) {
                        return cx.propagate();
                    }
                    this.review_files(window, cx)
                })
            })
            .child(summary)
            .child(
                Button::new(self.row_id("undo-files", span.first))
                    .plain(cx)
                    .xsmall()
                    .icon(Icon::new(IconName::Undo))
                    .label("Undo these")
                    .tooltip(undo_tip)
                    .disabled(self.busy || !undoable)
                    .on_click(cx.listener(move |this, _, window, cx| {
                        if !this.busy && this.store.conversations[this.store.active].id == session {
                            this.put_files_back(session, files.clone(), window, cx);
                        }
                    })),
            );
        Some(
            v_flex()
                .w_full()
                .rounded(theme.radius_lg)
                .border_1()
                .border_color(theme.border)
                .child(line)
                .children(rows)
                .into_any_element(),
        )
    }

    /// Show the Changes tab on this session's files, which the card is
    /// about, whichever list it was last switched to.
    fn review_files(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.switch_scope(crate::ui::changes::Scope::Session, cx);
        self.show_changes(window, cx);
    }

    /// One file of the card: its path and the lines the turn added and
    /// removed, or why they cannot be told. Enter or a click opens its diff.
    fn changed_file_row(
        &self,
        n: usize,
        file: &history::Changed,
        path: &str,
        shown: bool,
        focus: &FocusHandle,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let theme = cx.theme();
        let (muted, green, danger) = (theme.muted_foreground, theme.green, theme.danger);
        let lines = file.lines.as_ref();
        let why = match file.restore.fate {
            history::Fate::Changed => "changed since",
            history::Fate::NoCopy => "no copy kept",
            history::Fate::Outside => "outside the project",
            history::Fate::Back | history::Fate::Remove => "not text",
        };
        h_flex()
            .id(self.row_id("changed-file", n))
            .track_focus(focus)
            .key_context("ChangedFile")
            .h(px(26.))
            .min_w_0()
            .w_full()
            .px_2()
            .gap_1p5()
            .items_center()
            .text_xs()
            .when(lines.is_some(), |row| row.cursor_pointer())
            .focus(|s| s.bg(theme.secondary))
            .child(
                Icon::new(if shown {
                    IconName::ChevronDown
                } else {
                    IconName::ChevronRight
                })
                .size_3()
                .text_color(if lines.is_some() { muted } else { theme.border }),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .font_family(MONO_FONT)
                    .child(path.to_owned()),
            )
            .map(|row| match lines {
                Some(lines) => row
                    .child(div().text_color(green).child(format!("+{}", lines.added)))
                    .child(
                        div()
                            .text_color(danger)
                            .child(format!("−{}", lines.removed)),
                    ),
                None => row.child(div().text_color(muted).child(why)),
            })
            // As on a tool row, a press leaves the keys in the composer;
            // Tab is what picks the file out.
            .on_any_mouse_down(|_, window, _| window.prevent_default())
            // Enter is ToggleDiff's: gpui also clicks a focused element as
            // the key comes up, which closed the diff straight away.
            .on_click(cx.listener(move |this, event: &ClickEvent, _, cx| {
                if !event.is_keyboard() {
                    this.toggle_diff(n, cx)
                }
            }))
            .on_action(cx.listener(move |this, _: &ToggleDiff, _, cx| this.toggle_diff(n, cx)))
    }

    /// Open or close the diff of the card's `n`th file, if it has one.
    fn toggle_diff(&mut self, n: usize, cx: &mut Context<Self>) {
        let Some(card) = self.turn_files.as_mut() else {
            return;
        };
        if card.files.get(n).is_none_or(|file| file.lines.is_none()) {
            return;
        }
        if card.open.remove(&n).is_none() {
            card.open.insert(n, ScrollHandle::new());
        }
        cx.notify();
    }
}

/// A file's diff under its row: additions and removals in their colours,
/// long unchanged stretches as a count, scrolling past `DIFF_SHOWN` lines.
fn diff_rows(
    id: ElementId,
    lines: &history::Lines,
    scroll: &ScrollHandle,
    cx: &Context<Chat>,
) -> impl IntoElement {
    let theme = cx.theme();
    let row = |mark: &str, text: &str, color: Hsla| {
        let text: String = text.chars().take(DIFF_LINE).collect();
        // Clipped, not truncated: gpui keeps the first width a truncated
        // line was measured at, here too narrow, and drew every line as "…".
        div()
            .overflow_hidden()
            .whitespace_nowrap()
            .text_color(color)
            .child(format!("{mark} {}", text.replace('\t', "    ")))
    };
    let hidden = lines.rows.len().saturating_sub(DIFF_ROWS);
    let body = v_flex()
        .min_w_0()
        .text_xs()
        .font_family(MONO_FONT)
        .line_height(crate::ui::CODE_LINE)
        .children(lines.rows.iter().take(DIFF_ROWS).map(|line| match line {
            history::Row::Same(text) => row(" ", text, theme.muted_foreground),
            history::Row::Added(text) => row("+", text, theme.green),
            history::Row::Removed(text) => row("-", text, theme.danger),
            history::Row::Folded(n) => row(
                " ",
                &format!("⋯ {n} unchanged {}", if *n == 1 { "line" } else { "lines" }),
                theme.muted_foreground,
            ),
        }))
        .when(hidden > 0, |this| {
            this.child(row(
                " ",
                &format!("⋯ {hidden} more"),
                theme.muted_foreground,
            ))
        });
    let frame = div()
        .ml_4()
        .mb_2()
        .pl_3()
        .min_w_0()
        .border_l_2()
        .border_color(theme.border);
    // Each row is one line high, clipped rather than wrapped, so the count
    // is the height.
    let drawn = lines.rows.len().min(DIFF_ROWS) + usize::from(hidden > 0);
    if !scrolls(drawn) {
        return frame.child(body);
    }
    frame.child(
        div()
            .id(id)
            .relative()
            // A definite height, so the scroll area inside has something to
            // resolve `size_full` against.
            .h(crate::ui::CODE_LINE * DIFF_SHOWN)
            .child(
                div()
                    .id("diff-scroll")
                    .occlude()
                    .size_full()
                    .overflow_y_scroll()
                    .track_scroll(scroll)
                    .child(body),
            )
            .vertical_scrollbar(scroll),
    )
}

/// Whether a diff of `drawn` lines is taller than an open diff is let be.
fn scrolls(drawn: usize) -> bool {
    drawn as f32 > DIFF_SHOWN
}

/// What a turn added and removed in all, from its files' counts; `None`
/// when no file's lines can be told.
fn line_totals<'a>(
    lines: impl IntoIterator<Item = Option<&'a history::Lines>>,
) -> Option<(usize, usize)> {
    lines.into_iter().flatten().fold(None, |sum, lines| {
        let (added, removed) = sum.unwrap_or((0, 0));
        Some((added + lines.added, removed + lines.removed))
    })
}

/// Whether a turn shows its buttons: the newest one once nothing is coming
/// in, since it is the one most often asked again, and the one that holds
/// focus, picked out with Tab or by selecting its text.
pub(crate) fn shows_bar(span: Span, messages: usize, streaming: bool, focused: bool) -> bool {
    focused || (!streaming && span.last + 1 == messages)
}

/// Where Alt+Up and Alt+Down stop among `turns`: each with a menu, which
/// leaves out a notice and, while `streaming`, the turn the reply is coming
/// in to; then, while `asking`, the question on that turn's card.
fn stops(turns: &[Span], messages: &[Message], streaming: bool, asking: bool) -> Vec<Stop> {
    let live = turns.len().checked_sub(1).filter(|_| streaming);
    let mut stops: Vec<Stop> = turns
        .iter()
        .enumerate()
        .filter(|&(n, span)| live != Some(n) && !messages.get(span.first).is_some_and(|m| m.notice))
        .map(|(_, span)| Stop::Turn(span.first))
        .collect();
    if asking {
        stops.push(Stop::Question);
    }
    stops
}

/// Where Alt+Up, or with `down` Alt+Down, goes among `count` stops from
/// the one `at`: no further than the ends, and from none to the newest.
fn step_stop(count: usize, at: Option<usize>, down: bool) -> Option<usize> {
    let last = count.checked_sub(1)?;
    Some(match at {
        None => last,
        Some(at) if down => (at + 1).min(last),
        Some(at) => at.saturating_sub(1),
    })
}

/// The session's turns, one per prompt as in Claude Code: your message on
/// its own, then everything the model did in reply to it. A reply still
/// coming in to your message is a turn already, with nothing in it yet.
pub(crate) fn turns(messages: &[Message], streaming: bool) -> Vec<Span> {
    let mut turns: Vec<Span> = Vec::new();
    let mut replied = false;
    for (i, m) in messages.iter().enumerate() {
        let said = m.role == "user";
        match turns.last_mut() {
            Some(turn) if !said && replied => turn.last = i,
            // A notice stands apart as your message does, but it is
            // nobody's prompt to change or ask again.
            _ => turns.push(Span {
                first: i,
                last: i,
                yours: said && !m.notice,
            }),
        }
        replied = !said;
    }
    if streaming && !replied {
        turns.push(Span {
            first: messages.len(),
            last: messages.len(),
            yours: false,
        });
    }
    turns
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
    use super::{
        Span, Stop, line_totals, message_count, scrolls, shows_bar, step_stop, stops, turns,
    };
    use letronna_core::chat::Message;
    use letronna_core::history::Lines;

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
    fn alt_steps_stop_at_the_ends_and_start_from_the_newest() {
        assert_eq!(step_stop(4, None, false), Some(3));
        assert_eq!(step_stop(4, None, true), Some(3));
        assert_eq!(step_stop(4, Some(2), false), Some(1));
        assert_eq!(step_stop(4, Some(0), false), Some(0));
        assert_eq!(step_stop(4, Some(2), true), Some(3));
        assert_eq!(step_stop(4, Some(3), true), Some(3));
        assert_eq!(step_stop(0, None, false), None);
    }

    #[test]
    fn every_turn_with_a_menu_is_a_stop_drawn_or_not_and_the_question_last() {
        let said = |role: &str, notice| Message {
            role: role.into(),
            notice,
            ..Default::default()
        };
        let session = [
            said("user", false),
            said("assistant", false),
            said("user", true),
            said("assistant", false),
            said("user", false),
        ];
        // The notice is no place to go back to.
        let finished = turns(&session, false);
        assert_eq!(
            stops(&finished, &session, false, false),
            [Stop::Turn(0), Stop::Turn(1), Stop::Turn(3), Stop::Turn(4)]
        );
        // The turn a reply is coming in to is not one yet; its card is.
        let replying = turns(&session, true);
        assert_eq!(
            stops(&replying, &session, true, true),
            [
                Stop::Turn(0),
                Stop::Turn(1),
                Stop::Turn(3),
                Stop::Turn(4),
                Stop::Question
            ]
        );
        assert!(stops(&[], &[], false, false).is_empty());
    }

    #[test]
    fn the_line_adds_up_every_file_it_can_count() {
        let lines = |added, removed| Lines {
            added,
            removed,
            ..Default::default()
        };
        let (small, large) = (lines(3, 1), lines(37, 11));
        // A file whose lines cannot be told adds nothing, and no other.
        assert_eq!(
            line_totals([Some(&small), None, Some(&large)]),
            Some((40, 12))
        );
        assert_eq!(line_totals([None, None]), None);
        assert_eq!(line_totals([]), None);
        // Counted as nothing is still counted: the line says +0 −0.
        assert_eq!(line_totals([Some(&lines(0, 0))]), Some((0, 0)));
    }

    #[test]
    fn a_diff_scrolls_only_past_twelve_lines() {
        assert!(!scrolls(1));
        assert!(!scrolls(12));
        assert!(scrolls(13));
        assert!(scrolls(301));
    }

    #[test]
    fn counts_read_as_words() {
        assert_eq!(message_count(1), "1 message");
        assert_eq!(message_count(3), "3 messages");
    }

    #[test]
    fn a_turn_is_your_message_or_everything_the_model_did_after_it() {
        let said = |role: &str| Message {
            role: role.into(),
            ..Default::default()
        };
        let session = [
            said("user"),
            said("assistant"),
            said("tool"),
            said("assistant"),
            said("user"),
        ];
        let span = |first, last, yours| Span { first, last, yours };
        assert_eq!(
            turns(&session, false),
            [span(0, 0, true), span(1, 3, false), span(4, 4, true)]
        );
        // The reply coming in to the last prompt is a turn of its own...
        assert_eq!(turns(&session, true)[3], span(5, 5, false));
        // ...but a reply that is already there carries on in its own.
        assert_eq!(turns(&session[..4], true).len(), 2);
        assert!(turns(&[], false).is_empty());
        // A notice is a turn of its own too, not yours, and the reply to it
        // another.
        let notice = Message {
            role: "user".into(),
            notice: true,
            ..Default::default()
        };
        let woken = [said("user"), said("assistant"), notice, said("assistant")];
        assert_eq!(
            turns(&woken, false),
            [
                span(0, 0, true),
                span(1, 1, false),
                span(2, 2, false),
                span(3, 3, false)
            ]
        );
        assert_eq!(turns(&woken[..3], true)[3], span(3, 3, false));
    }
}
